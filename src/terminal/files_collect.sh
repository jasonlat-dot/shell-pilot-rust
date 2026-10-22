# ShellPilot terminal SFTP
d='__BRIDGE__'; r='__REQUEST__'; sender=
cleanup() {
    [ -z "$sender" ] || kill "$sender" 2>/dev/null
    timeout 1 sh -c 'printf "D %s\n" "$1" > "$2/requests"' sh "$r" "$d" >/dev/null 2>&1
}
trap cleanup EXIT
trap 'exit 125' HUP INT TERM
test -p "$d/requests" || exit 125
printf 'S %s\n' "$r" > "$d/requests" || exit 125
i=0
while [ ! -f "$d/$r.ready" ]; do
    i=$((i+1)); [ "$i" -lt 150 ] || exit 125
    sleep 0.02
done
[ "$(cat "$d/$r.ready")" = OK ] || exit 125
printf 'SHELLPILOT_TERMINAL_SFTP_READY\n'
exec 3<&0
cat <&3 > "$d/$r.in" & sender=$!
printf 'A %s\n' "$r" > "$d/requests" || exit 125
cat "$d/$r.out"
