#!/bin/bash
set -euo pipefail

# Deploy cpfp.me to a fresh server
# Run as root on the target server

echo "=== Installing Docker ==="
if ! command -v docker &>/dev/null; then
    curl -fsSL https://get.docker.com | sh
fi

echo "=== Setting up cpfp-me ==="
cd /opt/cpfp-me

# First run: phoenixd needs to start once to generate its config
if [ ! -f .env ]; then
    echo "Starting phoenixd to generate config..."
    docker compose -f compose.prod.yaml up -d phoenixd
    sleep 10

    # Extract phoenixd password
    PHOENIXD_PW=$(docker compose -f compose.prod.yaml exec phoenixd \
        grep '^http-password=' /phoenix/.phoenix/phoenix.conf | cut -d= -f2)

    echo "Phoenixd password: $PHOENIXD_PW"
    echo ""
    echo "Create .env file with your secrets:"
    echo "  CPFP_MNEMONIC=\"your 12 or 24 word mnemonic\""
    echo "  CPFP_PHOENIXD_PASSWORD=\"$PHOENIXD_PW\""
    echo ""
    echo "Then run: docker compose -f compose.prod.yaml up -d"
    exit 0
fi

echo "=== Building and starting ==="
docker compose -f compose.prod.yaml up -d --build

echo ""
echo "=== Status ==="
docker compose -f compose.prod.yaml ps
echo ""
echo "Service should be available at http://$(hostname -I | awk '{print $1}')/"
