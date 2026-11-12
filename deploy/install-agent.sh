#!/usr/bin/env sh
# pdman-agent kurulum betiği (Linux).
#
# Kullanım:
#   ./install-agent.sh                    # son sürümü GitHub'dan indirir
#   ./install-agent.sh /yol/pdman-agent   # derlenmiş binary'yi kurar
#
# Ne yapar:
#   1. Binary'yi /usr/local/bin/pdman-agent olarak kurar
#   2. /etc/pdman-agent dizinini ve örnek yapılandırmayı oluşturur
#   3. systemd servisini kurup etkinleştirir

set -eu

BINARY_DEST=/usr/local/bin/pdman-agent
CONFIG_DIR=/etc/pdman-agent
CONFIG_FILE=$CONFIG_DIR/pdman-agent.toml
SERVICE_SRC="$(dirname "$0")/pdman-agent.service"
SERVICE_DEST=/etc/systemd/system/pdman-agent.service

if [ "$(id -u)" -ne 0 ]; then
    echo "HATA: kök kullanıcı olarak çalıştırın (sudo)." >&2
    exit 1
fi

# --- 1. Binary ---
if [ "${1:-}" != "" ]; then
    # Yerel binary kurulur.
    install -m 0755 "$1" "$BINARY_DEST"
else
    # Son sürüm GitHub release'lerinden indirilir.
    ARCH=$(uname -m)
    case "$ARCH" in
        x86_64) TARGET="x86_64-unknown-linux-musl" ;;
        aarch64 | arm64) TARGET="aarch64-unknown-linux-musl" ;;
        *)
            echo "HATA: desteklenmeyen mimari: $ARCH" >&2
            exit 1
            ;;
    esac

    URL="https://github.com/kullanici/pdman/releases/latest/download/pdman-agent-$TARGET.tar.gz"
    echo "İndiriliyor: $URL"
    curl -fsSL "$URL" | tar -xz -C /tmp
    install -m 0755 "/tmp/pdman-agent" "$BINARY_DEST"
    rm -f /tmp/pdman-agent
fi

echo "Kuruldu: $BINARY_DEST ($($BINARY_DEST --version))"

# --- 2. Yapılandırma ---
mkdir -p "$CONFIG_DIR"

if [ ! -f "$CONFIG_FILE" ]; then
    cat > "$CONFIG_FILE" <<'EOF'
name = "bu-sunucu"

[server]
bind = "0.0.0.0:7654"

[auth]
# ÖNEMLİ: 0.0.0.0'a bind için token zorunlu. Değiştirin!
token = "DEGISTIR-BEN"

[docker]
socket = "/var/run/docker.sock"

[podman]
socket = "/run/podman/podman.sock"
EOF
    chmod 0640 "$CONFIG_FILE"
    echo "Örnek yapılandırma oluşturuldu: $CONFIG_FILE (token'ı değiştirin!)"
else
    echo "Yapılandırma zaten var, dokunulmadı: $CONFIG_FILE"
fi

# --- 3. systemd servisi ---
if [ -f "$SERVICE_SRC" ]; then
    install -m 0644 "$SERVICE_SRC" "$SERVICE_DEST"
    systemctl daemon-reload
    systemctl enable --now pdman-agent
    echo "Servis kuruldu ve başlatıldı: systemctl status pdman-agent"
else
    echo "Not: $SERVICE_SRC bulunamadı; systemd servisi kurulmadı."
    echo "      Binary'yi elle çalıştırabilirsiniz: pdman-agent --config $CONFIG_FILE"
fi
