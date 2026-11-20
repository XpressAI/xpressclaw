"""XpressCLAW's header-only mitmproxy gateway. No flow or body recording."""
import asyncio
import json
import os
import re
import signal
import sys
import time
from pathlib import Path

from mitmproxy import ctx, http, options
from mitmproxy.tools.dump import DumpMaster

POLICY = Path('/policy/policy.json')
EMAIL = re.compile(r'[A-Za-z0-9.!#$%&\x27*+/=?^_`{|}~-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+')


def host_matches(host, pattern):
    host, pattern = host.lower().rstrip('.'), pattern.lower()
    if pattern.startswith('*.'):
        return host.endswith(pattern[1:]) and host != pattern[2:]
    return host == pattern


def listed(host, patterns):
    return any(host_matches(host, pattern) for pattern in patterns)


def timeout_handler(_signum, _frame):
    raise TimeoutError('header match exceeded its time budget')


def matches(rule, value):
    kind = rule['matcher']
    if kind == 'any':
        return True
    if kind == 'exact':
        return value == rule['value']
    if kind == 'contains':
        return rule['value'] in value
    pattern = EMAIL if kind == 'email' else re.compile(rule['value'])
    signal.setitimer(signal.ITIMER_REAL, 0.025)
    try:
        return pattern.search(value) is not None
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


def validate(config):
    for rule in config['header_rules']:
        if rule['matcher'] == 'regex':
            re.compile(rule['value'])


def event(action, host='', rule=None, peer=None):
    # Only structured metadata leaves this process. Never log URLs or values.
    print(json.dumps({'proxy_event': True, 'time': int(time.time()),
                      'action': action, 'host': host, 'rule': rule,
                      'client': peer[0] if peer else None}), flush=True)


class Policy:
    def __init__(self):
        self.config = None
        self.stamp = None

    def load_policy(self):
        stamp = POLICY.stat().st_mtime_ns
        if stamp != self.stamp:
            config = json.loads(POLICY.read_text())
            validate(config)
            self.config, self.stamp = config, stamp
            # Escaped, anchored hostname patterns; never user-supplied regex.
            patterns = []
            ports = '|'.join(str(port) for port in config.get('web_ports', [80, 443]))
            for host in config['inspection_bypass']:
                suffix = re.escape(host[2:] if host.startswith('*.') else host)
                patterns.append('(?i)^' + (r'(?:[^.:]+\.)+' if host.startswith('*.') else '') + suffix + r'\.?\:(?:' + ports + ')$')
            ctx.options.update(ignore_hosts=patterns)
        return self.config

    def reject(self, flow, action, rule=None):
        event(action, flow.request.host, rule, flow.client_conn.peername)
        if flow.request.stream and (flow.request.headers.get('transfer-encoding') or int(flow.request.headers.get('content-length', '0')) > 0):
            # mitmproxy cannot synthesize a response while streaming a request.
            # Close blocked uploads before buffering or forwarding their bodies.
            flow.kill()
            return
        flow.request.stream = False
        flow.response = http.Response.make(403, 'Blocked by XpressCLAW proxy policy: ' + action,
                                          {'Content-Type': 'text/plain', 'X-XpressCLAW-Proxy': action})

    def permitted(self, flow):
        try:
            config = self.load_policy()
        except Exception:
            self.reject(flow, 'policy_unavailable')
            return None
        host = flow.request.host
        if flow.request.port not in config.get('web_ports', [80, 443]):
            self.reject(flow, 'blocked_proxy_port')
            return None
        if listed(host, config['blocked_domains']):
            self.reject(flow, 'blocked_domain')
            return None
        if config['domain_policy'] == 'allow_listed_only' and not listed(host, config['allowed_domains']):
            self.reject(flow, 'not_allowlisted')
            return None
        return config

    def http_connect(self, flow):
        # This runs before TLS passthrough, so exemptions cannot override denial.
        self.permitted(flow)

    def requestheaders(self, flow):
        flow.request.stream = True
        config = self.permitted(flow)
        if config is None or listed(flow.request.host, config['inspection_bypass']):
            return
        for index, rule in enumerate(config['header_rules']):
            if rule['domains'] and not listed(flow.request.host, rule['domains']):
                continue
            values = flow.request.headers.get_all(rule['header'])
            try:
                matched = any(matches(rule, value) for value in values)
            except (TimeoutError, re.error):
                self.reject(flow, 'header_match_failed', index + 1)
                return
            if not matched:
                continue
            action = rule['action']
            if action == 'block':
                self.reject(flow, 'blocked_header', index + 1)
                return
            # A match sanitizes every duplicate of the named header.
            del flow.request.headers[rule['header']]
            if action == 'replace':
                flow.request.headers[rule['header']] = rule['replacement']
            event('header_' + action, flow.request.host, index + 1, flow.client_conn.peername)

    def responseheaders(self, flow):
        if flow.response.raw_content is None:
            flow.response.stream = True

    def running(self):
        self.load_policy()
        event('ready')


async def main():
    signal.signal(signal.SIGALRM, timeout_handler)
    opts = options.Options(listen_host='0.0.0.0', listen_port=8080, confdir='/ca')
    master = DumpMaster(opts, with_termlog=False, with_dumper=False)
    # Upstream verification stays enabled. Non-HTTP protocols do not become tunnels.
    opts.update(connection_strategy='lazy', rawtcp=False, http3=False, ssl_insecure=False)
    master.addons.add(Policy())
    # Runner callbacks have a fixed destination selected by the control plane.
    # The existing callback listener remains responsible for authentication.
    async def callback(reader, writer):
        upstream = None
        try:
            target = json.loads(Path('/policy/callback.json').read_text())
            remote_reader, upstream = await asyncio.wait_for(
                asyncio.open_connection(target['host'], target['port']), 10)

            async def copy(source, destination):
                while chunk := await source.read(65536):
                    destination.write(chunk)
                    await destination.drain()

            tasks = [asyncio.create_task(copy(reader, upstream)),
                     asyncio.create_task(copy(remote_reader, writer))]
            try:
                await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
        except (OSError, ValueError, KeyError, asyncio.TimeoutError):
            pass
        finally:
            writer.close()
            if upstream:
                upstream.close()

    relay = await asyncio.start_server(callback, '0.0.0.0', 9080)
    async with relay:
        await master.run()


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--validate':
        validate(json.loads(sys.argv[2]))
    else:
        asyncio.run(main())
