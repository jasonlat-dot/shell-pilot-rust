# SHELLPILOT_RESOURCES_V1: fixed read-only probe, no PTY, sudo, install or user input.
LC_ALL=C
export LC_ALL
printf 'SHELLPILOT_RESOURCES_V1\n'
if [ ! -r /proc/stat ] || [ ! -r /proc/uptime ]; then
  printf 'UNSUPPORTED\n'
  exit 0
fi
printf '[stat]\n'
awk '$1 == "cpu" || $1 == "btime" { print }' /proc/stat 2>/dev/null
printf '[memory]\n'
awk '/^(MemTotal|MemAvailable|MemFree|Buffers|Cached|SReclaimable|Shmem):/ { print }' /proc/meminfo 2>/dev/null
printf '[uptime]\n'
cat /proc/uptime 2>/dev/null
printf '\n[network]\n'
cat /proc/net/dev 2>/dev/null
printf '\n[route]\n'
cat /proc/net/route 2>/dev/null
printf '\n[disks]\n'
df -P -k -l 2>/dev/null
printf '\n[end]\n'
