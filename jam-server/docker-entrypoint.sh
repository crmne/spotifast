#!/bin/sh
# Makes the server's certificate on first start, then runs the command given
# (`serve` by default, or `code --host <address>`) on the /data volume.
set -eu

DATA=/data
if [ ! -f "$DATA/cert.pem" ] || [ ! -f "$DATA/key.pem" ]; then
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
        -days 3650 -subj /CN=spotifast-jam \
        -keyout "$DATA/key.pem" -out "$DATA/cert.pem" 2>/dev/null
    chmod 600 "$DATA/key.pem"
fi
exec jam-server --data "$DATA" "$@"
