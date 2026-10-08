# Общее для сквозных проверок (подключается через source после T=…).
#
# Каталог прогона во временном каталоге: копии бинарников (приложение,
# поддельный сервер) удаляются всегда, сам каталог — при успехе. При ошибке
# или с NUNCIO_KEEP=1 он остаётся со снимками и журналами для разбора.
# Без этого каталоги копились в /tmp до исчерпания квоты.
tmp_done() {
  local rc=$1
  rm -f "$T/app" "$T/fakeserver"
  if [ "$rc" = 0 ] && [ -z "${NUNCIO_KEEP:-}" ]; then
    rm -rf "$T"
  else
    echo "каталог прогона: $T" >&2
  fi
}

# Уведомления рабочего стола в проверках — только на своей шине (notifytest),
# иначе выключены: на рабочий стол человека ничего не попадает.
export NUNCIO_DESKTOP_BUS=${NUNCIO_DESKTOP_BUS:-off}

# private_bus КАТАЛОГ — своя шина D-Bus без автозапуска служб (иначе
# dbus-daemon --session поднимает порталы и прочее из сессии человека).
# Адрес — в КАТАЛОГ/bus.addr, pid — в BUS.
private_bus() {
  cat > "$1/bus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=$1/bus</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
CONF
  dbus-daemon --config-file="$1/bus.conf" --nofork --nopidfile --print-address=3 3> "$1/bus.addr" &
  BUS=$!
  until [ -s "$1/bus.addr" ]; do sleep 0.1; done
}

# Ссылки в проверках не открываются в браузере человека: адреса — в файл.
export NUNCIO_TEST_OPEN=${NUNCIO_TEST_OPEN:-$T/opened.txt}

# Звуки уведомлений в проверках не выводятся (только строка в журнале).
export NUNCIO_TEST_SILENT=1
