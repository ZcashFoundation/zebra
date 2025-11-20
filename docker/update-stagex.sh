#!/bin/sh
cp Dockerfile Dockerfile.bak
for s in core user bootstrap; do
    curl -sL https://codeberg.org/stagex/stagex/raw/branch/main/digests/$s.txt |
    while read d n; do 
        sed -i "s|FROM stagex/${n}@sha256:[^ ]* AS|FROM stagex/$n@sha256:$d AS|" Dockerfile
    done
done
rm Dockerfile.bak
