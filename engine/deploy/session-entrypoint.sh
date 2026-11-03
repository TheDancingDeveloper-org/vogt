#!/bin/sh
set -eu
# Placeholder entrypoint for WI-3162. WI-3163 owns session startup.
if [ "$#" -eq 0 ]; then
  set -- /bin/bash
fi
exec "$@"
