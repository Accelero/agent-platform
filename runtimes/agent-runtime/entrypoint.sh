#!/bin/sh
set -eu
mkdir -p /home/worker/hostkeys
if [ -f /etc/ssh/hostkeys/ssh_host_ed25519_key ]; then
 # managed-ssh-v2: the manager provisions the server identity independently of
 # the client key. Copy the read-only projection into a private 0600 file.
 umask 077
 cp /etc/ssh/hostkeys/ssh_host_ed25519_key /home/worker/hostkeys/ssh_host_ed25519_key
 chmod 600 /home/worker/hostkeys/ssh_host_ed25519_key
elif [ ! -f /home/worker/hostkeys/ssh_host_ed25519_key ]; then
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
