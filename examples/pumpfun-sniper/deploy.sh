#!/usr/bin/env bash
# Provision and run the pump.fun sniper on a fresh Ubuntu 24.04 box.
#
# Run this FROM YOUR OWN MACHINE (it ssh's to the target), or copy it to the
# box and run it there with SNIPER_HOST=local.
#
#   SNIPER_HOST=root@1.2.3.4 ./deploy.sh provision   # deps, rust, chrony, build
#   SNIPER_HOST=root@1.2.3.4 ./deploy.sh install     # systemd unit (simulate)
#   SNIPER_HOST=root@1.2.3.4 ./deploy.sh logs        # follow the journal
#
# Deliberately does NOT write a .env with live values or flip SEND_MODE off
# simulate — do both by hand once you have looked at the simulate output.
set -euo pipefail

REPO_URL="${REPO_URL:-https://github.com/mekka-tech/carbon.git}"
BRANCH="${BRANCH:-sniper}"
DIR="${DIR:-/root/carbon}"
HOST="${SNIPER_HOST:?set SNIPER_HOST=root@<ip>, or SNIPER_HOST=local to run on the box}"

run() {
  if [ "$HOST" = "local" ]; then bash -lc "$1"; else ssh "$HOST" "$1"; fi
}

provision() {
  # chrony is not cosmetic: the freshness gate compares each create's block_time
  # against the local clock, so drift silently kills or admits snipes.
  run 'set -e
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y build-essential pkg-config libssl-dev protobuf-compiler \
                       git curl chrony
    systemctl enable --now chrony
    command -v cargo >/dev/null || {
      curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    }'
  run "set -e
    . \$HOME/.cargo/env
    if [ -d '$DIR/.git' ]; then
      cd '$DIR' && git fetch origin '$BRANCH' && git checkout '$BRANCH' && git pull origin '$BRANCH'
    else
      git clone --branch '$BRANCH' '$REPO_URL' '$DIR'
    fi
    cd '$DIR'
    cargo test -p pumpfun-sniper-example
    cargo build --release -p pumpfun-sniper-example
    test -f .env || cp examples/pumpfun-sniper/.env.example .env
    echo
    echo 'Built. Now edit $DIR/.env — at minimum GEYSER_URL, RPC_URLS,'
    echo 'WATCHED_CREATORS and BUYER_KEYPAIR_DIR. It stays on SEND_MODE=simulate'
    echo 'until you change it.'
    chronyc tracking | head -3"
}

install_unit() {
  run "cat > /etc/systemd/system/pumpfun-sniper.service <<UNIT
[Unit]
Description=pump.fun creator-wallet sniper
After=network-online.target chrony.service
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$DIR
EnvironmentFile=$DIR/.env
Environment=RUST_LOG=info
ExecStart=$DIR/target/release/pumpfun-sniper-example
Restart=always
RestartSec=2
# 30 wallets x several send paths opens a lot of sockets at once.
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable pumpfun-sniper
  echo 'Unit installed but NOT started. Check \$DIR/.env, then:'
  echo '  systemctl start pumpfun-sniper && journalctl -u pumpfun-sniper -f'"
}

case "${1:-}" in
  provision) provision ;;
  install)   install_unit ;;
  logs)      run 'journalctl -u pumpfun-sniper -f -n 200' ;;
  *) echo "usage: SNIPER_HOST=root@<ip> $0 {provision|install|logs}" >&2; exit 2 ;;
esac
