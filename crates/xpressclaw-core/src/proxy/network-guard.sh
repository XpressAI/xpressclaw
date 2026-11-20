#!/bin/sh
# Runs only in an instance-owned network namespace, never in the host namespace.
set -eu
umask 077
mkdir -p /run/xpressclaw-network
cd /run/xpressclaw-network

fail_closed() {
    rm -f ready
    for firewall in iptables ip6tables; do
        "$firewall" -w 5 -P OUTPUT DROP 2>/dev/null || true
        "$firewall" -w 5 -F OUTPUT 2>/dev/null || true
    done
}
trap fail_closed EXIT
trap 'exit 0' TERM INT

# Capture the engine's resolvers before an Agent joins the namespace. Later
# edits to a runner's /etc/hosts or resolv.conf cannot expand firewall access.
awk '$1 == "nameserver" { print $2 }' /etc/resolv.conf > resolvers
awk '$1 == "search" { for (i=2; i<=NF; i++) { sub(/\.$/, "", $i); if (length($i)) print $i } }' /etc/resolv.conf > search-domains
cp /etc/hosts engine-hosts

family_file() {
    case "$1" in *:*) printf '%s' rules.v6 ;; *) printf '%s' rules.v4 ;; esac
}

allow() {
    protocol=$1 address=$2 ports=$3
    printf -- '-A OUTPUT -p %s -d %s --dport %s -j ACCEPT\n' "$protocol" "$address" "$ports" >> "$(family_file "$address")"
}

base_rules() {
    for table in rules.v4 rules.v6; do
        cat > "$table" <<'RULES'
*filter
:INPUT ACCEPT [0:0]
:FORWARD DROP [0:0]
:OUTPUT DROP [0:0]
-A OUTPUT -o lo -j ACCEPT
-A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
RULES
    done
    while read -r resolver; do
        allow udp "$resolver" 53
        allow tcp "$resolver" 53
    done < resolvers
}

install_rules() {
    printf 'COMMIT\n' >> rules.v4
    printf 'COMMIT\n' >> rules.v6
    iptables-restore -w 5 < rules.v4
    ip6tables-restore -w 5 < rules.v6
}

# No Agent is started until the final policy, including both address families,
# is installed. Bootstrap permits only DNS while resolving configured targets.
base_rules
install_rules

resolve_target() {
    target=$1
    key=$(printf '%s' "$target" | sha256sum | awk '{print $1}')
    case "$target" in
        '*') printf '0.0.0.0/0\n::/0\n' > "dns.$key"; return ;;
        *:*|*/*) printf '%s\n' "$target" > "dns.$key"; return ;;
    esac
    # Preserve engine aliases such as host.docker.internal from the initial
    # namespace, before any Agent can edit the shared hosts file.
    awk -v target="$target" '{ for (i=2; i<=NF; i++) if (tolower($i)==target) print $1 }' engine-hosts > "dns.$key"
    [ ! -s "dns.$key" ] || return
    if printf '%s\n' "$target" | grep -Eq '^([0-9]+\.){3}[0-9]+$'; then
        printf '%s\n' "$target" > "dns.$key"
        return
    fi
    : > "dns.$key"
    # Absolute queries and the captured search list also ignore later edits to
    # the runner's resolver configuration.
    printf '%s.\n' "$target" > "queries.$key"
    case "$target" in *.*) ;; *)
        while read -r domain; do printf '%s.%s.\n' "$target" "$domain"; done < search-domains >> "queries.$key"
    ;; esac
    for query in A AAAA; do
        : > "answers.$key"
        while read -r hostname; do
            while read -r resolver; do
                if timeout 2 nslookup -type="$query" "$hostname" "$resolver" > "lookup.$key" 2>/dev/null; then
                    awk '/^Name:/ { answer=1; next } answer && /^Address:/ { print $2 }' "lookup.$key" >> "answers.$key"
                    break
                fi
            done < resolvers
            [ ! -s "answers.$key" ] || break
        done < "queries.$key"
        cat "answers.$key" >> "dns.$key"
    done
    sort -u "dns.$key" > "unique.$key"
    mv "unique.$key" "dns.$key"
    rm -f "lookup.$key" "queries.$key" "answers.$key"
}

last_hash=''
age=30
while :; do
    cp /policy/network.rules policy
    policy_hash=$(sha256sum policy | awk '{print $1}')
    if [ "$policy_hash" != "$last_hash" ] || [ "$age" -ge 30 ]; then
        proxy=$(awk '$1 == "proxy" { print $2 }' policy)
        [ -n "$proxy" ] || exit 1
        awk '$1 == "tcp" || $1 == "udp" { print $2 }' policy | sort -u > targets
        # Resolve unique destinations concurrently; an unreachable DNS target
        # contributes no allow rule and cannot stall the other destinations.
        while read -r destination; do resolve_target "$destination" & done < targets
        wait
        base_rules
        allow tcp "$proxy" 8080
        allow tcp "$proxy" 9080
        while read -r protocol destination ports; do
            case "$protocol" in tcp|udp) ;; *) continue ;; esac
            key=$(printf '%s' "$destination" | sha256sum | awk '{print $1}')
            while read -r address; do
                [ -n "$address" ] && allow "$protocol" "$address" "$ports"
            done < "dns.$key"
        done < policy
        install_rules
        printf '%s\n' "$policy_hash" > ready.new
        mv ready.new ready
        if [ "$policy_hash" != "$last_hash" ]; then
            printf '{"proxy_event":true,"time":%s,"action":"network_policy_applied","host":"","rule":null,"client":null}\n' "$(date +%s)"
        fi
        last_hash=$policy_hash
        age=0
    fi
    sleep 2 &
    wait "$!"
    age=$((age + 2))
done
