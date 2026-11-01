#!/bin/sh
set -eu
# Bind the SSH account to the Pod's identity without changing the root filesystem.
uid=$(id -u)
gid=$(id -g)
if [ "$uid" != 0 ]; then
 accounts=/tmp/accounts
 mkdir -p "$accounts"
 {
  awk -F: -v uid="$uid" '$1 != "worker" && $3 != uid' /etc/passwd
  echo "worker:*:$uid:$gid:worker:/home/worker:/bin/bash"
 } > "$accounts/passwd"
 {
  awk -F: -v gid="$gid" '$1 != "worker" && $3 != gid' /etc/group
  echo "worker:x:$gid:"
 } > "$accounts/group"
 export LD_PRELOAD=/usr/local/lib/libnss_wrapper.so \
  NSS_WRAPPER_PASSWD="$accounts/passwd" NSS_WRAPPER_GROUP="$accounts/group"
fi
mkdir -p /home/worker/hostkeys
if [ -f /etc/ssh/hostkeys/ssh_host_ed25519_key ]; then
 # Copy the projected host identity into a private file readable by sshd.
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
