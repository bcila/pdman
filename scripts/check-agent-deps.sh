#!/usr/bin/env sh
# pdman-agent'ın bağımlılık ağacında panel crate'i (pdman) olmamalıdır.
# Panel yalnızca `pdman` binary'sine derlenir; agent binary'sine asla girmez.
# Bkz. PLAN.md §1.7.

set -eu

if cargo tree -p pdman-agent --prefix none | awk '{print $1}' | sort -u | grep -qx "pdman"; then
    echo "HATA: pdman-agent bağımlılık ağacında panel crate'i (pdman) bulundu."
    echo "      pdman-agent yalnızca pdman-common'a bağımlı olmalı."
    exit 1
fi

echo "OK: pdman-agent bağımlılık ağacında panel crate'i yok."
