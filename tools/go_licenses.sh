#!/usr/bin/env bash
# Лицензии Go-модулей, попадающих в сборку (net/ffi): только разрешённые
# (BSD, MIT, Apache-2.0, ISC). Новый модуль с иной или неопознанной
# лицензией — ошибка; разрешённые модули перечислены и в third_party/NOTICES.md.
set -euo pipefail
cd "$(dirname "$0")/../net"
bad=0
while read -r path version dir; do
  [ "$path" = nuncio ] || [ -z "$dir" ] && continue
  file=$(find "$dir" -maxdepth 1 -iregex '.*/\(license\|licence\|copying\)[^/]*' | head -1)
  text=$(head -c 4000 "${file:-/dev/null}")
  case "$text" in
    *"Redistribution and use"*) kind=BSD ;;
    *"Permission is hereby granted, free of charge"*) kind=MIT ;;
    *"Apache License"*) kind=Apache-2.0 ;;
    *"Permission to use, copy, modify, and/or distribute"*) kind=ISC ;;
    *) kind="" ;;
  esac
  if [ -z "$kind" ]; then
    echo "ОШИБКА: $path $version — лицензия не опознана (${file:-нет файла})"
    bad=1
  else
    echo "$path $version: $kind"
  fi
done < <(go list -deps -f '{{if not .Standard}}{{with .Module}}{{.Path}} {{.Version}} {{.Dir}}{{end}}{{end}}' ./ffi | sort -u)
exit $bad
