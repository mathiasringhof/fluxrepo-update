#!/bin/sh
curl -sL {{server}}/example/flux/releases/latest/download/crd-schemas.tar.gz | tar zxf - -C /tmp/schema-cache
