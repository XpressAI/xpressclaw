"""Run with the pinned mitmproxy image; exercises real TLS without external sites."""
import asyncio
import datetime
import http.client
import importlib.util
import json
import signal
import socket
import ssl
import tempfile
import unittest
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID
from mitmproxy import options
from mitmproxy.tools.dump import DumpMaster

source = Path(__file__).resolve().parents[1] / 'crates/xpressclaw-core/src/proxy/gateway.py'
spec = importlib.util.spec_from_file_location('gateway', source)
gateway = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gateway)


def configuration():
    return dict(enabled=True, domain_policy='allow_except_blocked', allowed_domains=[],
                blocked_domains=[], inspection_bypass=[], header_rules=[dict(
                    header='User-Agent', matcher='email', value='', action='replace',
                    replacement='XpressCLAW/1.0', domains=[]), dict(
                    header='From', matcher='any', value='', action='remove', replacement='', domains=[])])


class GatewayTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        gateway.POLICY = self.root / 'policy.json'
        self.config = configuration()
        self.save()
        signal.signal(signal.SIGALRM, gateway.timeout_handler)
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'localhost')])
        now = datetime.datetime.now(datetime.timezone.utc)
        cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
                .public_key(key.public_key()).serial_number(x509.random_serial_number())
                .not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=1))
                .add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost')]), critical=False)
                .sign(key, hashes.SHA256()))
        self.cert = self.root / 'origin.pem'
        self.cert.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
        keyfile = self.root / 'origin.key'
        keyfile.write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(self.cert, keyfile)
        self.origin = await asyncio.start_server(self.echo, '127.0.0.1', 0, ssl=tls)
        self.origin_port = self.origin.sockets[0].getsockname()[1]
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            self.proxy_port = probe.getsockname()[1]
        opts = options.Options(listen_host='127.0.0.1', listen_port=self.proxy_port, confdir=str(self.root / 'ca'))
        self.master = DumpMaster(opts, with_termlog=False, with_dumper=False)
        opts.update(connection_strategy='lazy', ssl_verify_upstream_trusted_ca=str(self.cert), rawtcp=False, http3=False)
        self.master.addons.add(gateway.Policy())
        self.running = asyncio.create_task(self.master.run())
        for _ in range(100):
            if (self.root / 'ca/mitmproxy-ca-cert.pem').exists() and self.master.addons.get('proxyserver').servers:
                break
            await asyncio.sleep(.02)
        await asyncio.sleep(.1)

    def save(self):
        gateway.POLICY.write_text(json.dumps(self.config))

    async def asyncTearDown(self):
        self.master.shutdown()
        await self.running
        self.origin.close()
        await self.origin.wait_closed()
        self.temp.cleanup()

    async def echo(self, reader, writer):
        try:
            raw = await reader.readuntil(b'\r\n\r\n')
            headers = [line.split(':', 1) for line in raw.decode().split('\r\n')[1:] if ':' in line]
            length = next((int(value) for name, value in headers if name.lower() == 'content-length'), 0)
            body = await reader.readexactly(length)
            response = json.dumps({'headers': [[name.lower(), value.strip()] for name, value in headers], 'body': body.decode(), 'line': raw.decode().split('\r\n')[0]}).encode()
            writer.write(b'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ' + str(len(response)).encode() + b'\r\nConnection: close\r\n\r\n' + response)
            await writer.drain()
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()

    async def request(self, headers=None, passthrough=False, payload=b'body-address@example.invalid'):
        def send():
            ca = self.cert if passthrough else self.root / 'ca/mitmproxy-ca-cert.pem'
            conn = http.client.HTTPSConnection('127.0.0.1', self.proxy_port, context=ssl.create_default_context(cafile=str(ca)), timeout=5)
            conn.set_tunnel('localhost', self.origin_port)
            body = payload
            try:
                conn.putrequest('POST', '/echo?contact=url-address@example.invalid')
                conn.putheader('Content-Length', str(len(body)))
                for name, value in headers or [('User-Agent', 'Tool (person@example.invalid)'), ('From', 'person@example.invalid')]:
                    conn.putheader(name, value)
                conn.endheaders(body)
                response = conn.getresponse()
                return response.status, response.read()
            finally:
                conn.close()
        return await asyncio.to_thread(send)

    async def test_https_headers_sanitized_body_and_url_unchanged(self):
        status, raw = await self.request()
        self.assertEqual(status, 200)
        result = json.loads(raw)
        headers = dict(result['headers'])
        self.assertEqual(headers['user-agent'], 'XpressCLAW/1.0')
        self.assertNotIn('from', headers)
        self.assertEqual(result['body'], 'body-address@example.invalid')
        self.assertIn('url-address@example.invalid', result['line'])

    async def test_duplicate_headers_are_all_replaced(self):
        _, raw = await self.request([('user-agent', 'clean'), ('USER-AGENT', 'private@example.invalid')])
        values = [v for k, v in json.loads(raw)['headers'] if k == 'user-agent']
        self.assertEqual(values, ['XpressCLAW/1.0'])

    async def test_empty_allowlist_denies_connect(self):
        self.config['domain_policy'] = 'allow_listed_only'
        self.save()
        with self.assertRaisesRegex(OSError, '403'):
            await self.request()

    async def test_blocklist_wins_over_allow_and_inspection_exemption(self):
        self.config.update(allowed_domains=['localhost'], blocked_domains=['localhost'], inspection_bypass=['localhost'])
        self.save()
        with self.assertRaisesRegex(OSError, '403'):
            await self.request(passthrough=True)

    async def test_tls_passthrough_keeps_origin_certificate_and_headers(self):
        self.config['inspection_bypass'] = ['localhost']
        self.save()
        status, raw = await self.request(passthrough=True)
        self.assertEqual(status, 200)
        self.assertEqual(dict(json.loads(raw)['headers'])['user-agent'], 'Tool (person@example.invalid)')

    async def test_rules_reload_without_restarting(self):
        await self.request()
        self.config['header_rules'][0]['action'] = 'block'
        self.save()
        self.assertEqual((await self.request(payload=b''))[0], 403)
        with self.assertRaises(http.client.RemoteDisconnected):
            await self.request()

    async def test_rule_scope_and_regex(self):
        self.config['header_rules'][0].update(matcher='regex', value='person@.*', domains=['different.example'])
        self.save()
        _, raw = await self.request()
        self.assertIn('person@', dict(json.loads(raw)['headers'])['user-agent'])
        self.config['header_rules'][0]['domains'] = ['localhost']
        self.save()
        _, raw = await self.request()
        self.assertEqual(dict(json.loads(raw)['headers'])['user-agent'], 'XpressCLAW/1.0')

    async def test_missing_policy_fails_closed(self):
        gateway.POLICY.unlink()
        with self.assertRaisesRegex(OSError, '403'):
            await self.request()


class DomainTests(unittest.TestCase):
    def test_matching_is_not_substring_matching(self):
        for host, pattern, expected in [('EXAMPLE.com.', 'example.com', True), ('a.example.com', 'example.com', False), ('a.example.com', '*.example.com', True), ('example.com', '*.example.com', False), ('evilexample.com', '*.example.com', False), ('example.com.evil.test', '*.example.com', False)]:
            self.assertEqual(gateway.host_matches(host, pattern), expected)


if __name__ == '__main__':
    unittest.main()
