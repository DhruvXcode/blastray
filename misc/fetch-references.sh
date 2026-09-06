#!/bin/sh
set -eu

mkdir -p misc/references

fetch() {
    name=$1
    url=$2

    if [ -d "misc/references/$name/.git" ]; then
        git -C "misc/references/$name" pull --ff-only
    else
        git clone "$url" "misc/references/$name"
    fi
}

fetch gitnexus https://github.com/abhigyanpatwari/GitNexus
fetch codegraph https://github.com/colbymchenry/codegraph
fetch kodegraf https://github.com/DeRaowl/Kodegraf
fetch stockfish https://github.com/official-stockfish/Stockfish
