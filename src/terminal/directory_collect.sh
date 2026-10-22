# ShellPilot directory metadata
d='__BRIDGE__'; r='__REQUEST__'
test -p "$d/requests" || exit 125
printf 'R %s %s\n' "$r" '__PATH__' > "$d/requests" || exit 125
i=0
while [ ! -f "$d/$r.reply" ]; do
    i=$((i+1)); [ "$i" -lt 200 ] || exit 125
    sleep 0.02
done
cat -- "$d/$r.reply"; code=$?
timeout 1 sh -c 'printf "D %s\n" "$1" > "$2/requests"' sh "$r" "$d" >/dev/null 2>&1
exit "$code"
