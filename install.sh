#!/usr/bin/env bash
# Instala el binario `forja` desde este repo. Uso: ./install.sh
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
  echo "No encuentro 'cargo'. Instala Rust con https://rustup.rs y vuelve a ejecutar ./install.sh" >&2
  exit 1
fi
if ! command -v git >/dev/null 2>&1; then
  echo "No encuentro 'git'. Forja lo necesita para crear worktrees." >&2
  exit 1
fi

echo "Compilando e instalando forja (puede tardar unos minutos)..."
cargo install --path crates/forja-cli --locked --force

bin="${CARGO_HOME:-$HOME/.cargo}/bin"
case ":$PATH:" in
  *":$bin:"*) ;;
  *) echo "Aviso: $bin no está en tu PATH. Añádelo para usar 'forja' directamente." ;;
esac

echo
echo "Listo. Prueba:"
echo "  forja doctor        # revisa tu entorno"
echo "  cd tu-repo && forja serve --open"
