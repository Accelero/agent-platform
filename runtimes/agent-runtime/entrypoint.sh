#!/bin/sh
set -eu
mkdir -p /home/worker/hostkeys
if [ ! -f /home/worker/hostkeys/ssh_host_ed25519_key ]; then
 ssh-keygen -q -t ed25519 -N '' -f /home/worker/hostkeys/ssh_host_ed25519_key
fi
python3 /usr/local/lib/multica-checkout-proxy.py &
checkout_pid=$!
/usr/sbin/sshd -D -e &
sshd_pid=$!
trap 'kill "$checkout_pid" "$sshd_pid" 2>/dev/null || true' EXIT
trap 'exit 0' TERM INT
# Restart the container if either service exits.
while kill -0 "$checkout_pid" 2>/dev/null && kill -0 "$sshd_pid" 2>/dev/null; do
    sleep 1 &
    wait $! || true
done
exit 1
