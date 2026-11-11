#!/usr/bin/env bash
# Helpers for the microVM network isolation e2e tests (Cloud Hypervisor and
# Firecracker).
#
# A "nettest" guest runs one systemd unit that:
#   - listens on TCP 18080 (python3 http.server), so another guest can try it;
#   - prints `RING_NETTEST self <ip>` to the console every few seconds;
#   - when /etc/ring-nettest-targets exists, tries a TCP connection to each
#     `<name> <ip>:<port>` line in it (GATEWAY stands for the guest's default
#     gateway, the host side of its tap) and prints `RING_NETTEST <name>
#     open|closed`.
# The test reads those lines back with `ring deployment logs`.
#
# A guest whose cloud-init did not configure the network from Ring's cidata
# disk (a stock cloud-init only reads vfat or iso9660) gets it from the probe
# script, from the same disk.

NETTEST_GUEST_PORT=18080

_nettest_script() {
  cat <<'EOF'
#!/bin/bash
# A stock cloud-init ignores Ring's ext4 cidata disk: apply the network
# command Ring wrote into it, as cloud-init would have.
if ! ip -4 -o addr show scope global | grep -q inet; then
  mkdir -p /run/ring-cidata
  if mount -o ro LABEL=CIDATA /run/ring-cidata 2>/dev/null; then
    addr=$(grep -o 'ip addr add [0-9./]*' /run/ring-cidata/user-data | awk '{print $4}' | head -n1)
    via=$(grep -o 'default via [0-9.]*' /run/ring-cidata/user-data | awk '{print $3}' | head -n1)
    iface=$(ls /sys/class/net | grep -v '^lo$' | head -n1)
    if [ -n "$addr" ] && [ -n "$via" ] && [ -n "$iface" ]; then
      ip addr add "$addr" dev "$iface"
      ip link set "$iface" up
      ip route add default via "$via"
    fi
    umount /run/ring-cidata
  fi
fi
python3 -m http.server 18080 --bind 0.0.0.0 >/dev/null 2>&1 &
while true; do
  ip=$(ip -4 -o addr show scope global | awk '{print $4}' | cut -d/ -f1 | head -n1)
  gw=$(ip route show default | awk '{print $3}' | head -n1)
  echo "RING_NETTEST self ${ip:-none}" > /dev/console
  if [ -f /etc/ring-nettest-targets ] && [ -n "$gw" ]; then
    while read -r name addr; do
      [ -n "$name" ] || continue
      addr=${addr//GATEWAY/$gw}
      if timeout 3 bash -c "</dev/tcp/${addr%:*}/${addr##*:}" 2>/dev/null; then
        result=open
      else
        result=closed
      fi
      echo "RING_NETTEST $name $result" > /dev/console
    done < /etc/ring-nettest-targets
  fi
  sleep 5
done
EOF
}

_nettest_unit() {
  cat <<'EOF'
[Unit]
Description=Ring e2e network isolation probe

[Service]
Type=simple
ExecStart=/bin/bash /usr/local/bin/ring-nettest
Restart=always

[Install]
WantedBy=multi-user.target
EOF
}

# Usage: nettest_inject <ext4 filesystem image>
# Install the probe script and its unit into an ext4 filesystem image, in place.
nettest_inject() {
  local fs="$1"
  local work
  work=$(mktemp -d -t ring-e2e-nettest-XXXXXX)
  _nettest_script > "$work/ring-nettest"
  _nettest_unit > "$work/ring-nettest.service"

  # Replay the journal first: debugfs writes past it, and a pending replay at
  # mount would wipe what it injected (see ensure-job-image.sh).
  e2fsck -fy "$fs" >/dev/null 2>&1 || true
  {
    debugfs -w -R "write $work/ring-nettest /usr/local/bin/ring-nettest" "$fs"
    debugfs -w -R "write $work/ring-nettest.service /etc/systemd/system/ring-nettest.service" "$fs"
    debugfs -w -R "rm /etc/systemd/system/multi-user.target.wants/ring-nettest.service" "$fs"
    # Relative target: debugfs stores every symlink inline, which only holds
    # up to 59 bytes (see ensure-job-image.sh).
    debugfs -w -R "symlink /etc/systemd/system/multi-user.target.wants/ring-nettest.service ../ring-nettest.service" "$fs"
  } 2>&1 | grep -v "^debugfs " >&2 || true
  rm -rf "$work"
}

# Usage: nettest_set_targets <ext4 filesystem image> <targets file>
nettest_set_targets() {
  local fs="$1"
  local targets="$2"
  {
    debugfs -w -R "rm /etc/ring-nettest-targets" "$fs"
    debugfs -w -R "write $targets /etc/ring-nettest-targets" "$fs"
  } 2>&1 | grep -v "^debugfs " >&2 || true
}

# Usage: nettest_wait_line <deployment_id> <regex> [timeout_seconds]
# Print the last match of <regex> in the deployment's console output (the
# match only: `ring deployment logs` prefixes each line).
nettest_wait_line() {
  local id="$1"
  local pattern="$2"
  local timeout="${3:-120}"
  local line=""
  for _ in $(seq 1 "$timeout"); do
    line=$("$RING_BIN" deployment logs "$id" 2>/dev/null | tr -d '\r' | grep -oE "$pattern" | tail -n1 || true)
    if [ -n "$line" ]; then
      echo "$line"
      return 0
    fi
    sleep 1
  done
  fail "deployment $id printed no line matching '$pattern' in ${timeout}s"
}

# The host's address on its uplink: a host-private address a guest must not
# reach.
nettest_host_lan_ip() {
  ip -4 route get 1.1.1.1 | awk '{for (i = 1; i <= NF; i++) if ($i == "src") print $(i + 1)}' | head -n1
}

# Usage: nettest_start_host_listener <port>
# Serve TCP on every host address; prints the listener's pid.
nettest_start_host_listener() {
  local port="$1"
  python3 -m http.server "$port" --bind 0.0.0.0 >/dev/null 2>&1 &
  echo $!
}

# Usage: nettest_free_port
nettest_free_port() {
  python3 -c 'import socket; s = socket.socket(); s.bind(("", 0)); print(s.getsockname()[1])'
}

# Usage: nettest_on_partition <raw disk image> <function> [args...]
# Run `<function> <ext4 file> [args...]` on partition 1 of a partitioned raw
# disk image (the Cloud Hypervisor guest images), then write it back in place.
nettest_on_partition() {
  local disk="$1"
  shift
  local line start end work status=0
  line=$(parted -m -s "$disk" unit B print | awk -F: '$1=="1"{print}')
  [ -n "$line" ] || fail "no partition 1 in $disk"
  start=$(echo "$line" | awk -F: '{print $2}' | sed 's/B$//')
  end=$(echo "$line" | awk -F: '{print $3}' | sed 's/B$//')
  work=$(mktemp -d -t ring-e2e-nettest-part-XXXXXX)
  dd if="$disk" of="$work/part.ext4" bs=1M iflag=skip_bytes,count_bytes \
    skip="$start" count=$((end - start + 1)) status=none
  "$@" "$work/part.ext4" || status=$?
  dd if="$work/part.ext4" of="$disk" bs=1M oflag=seek_bytes seek="$start" \
    conv=notrunc status=none
  rm -rf "$work"
  return "$status"
}

# Argument order adapter for nettest_on_partition: the ext4 file comes last.
_nettest_set_targets_last() {
  nettest_set_targets "$2" "$1"
}
