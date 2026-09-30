#!/bin/sh
# Assemble the upload-ready bundle for the xerj-decide model hub repo.
#
# There is no HuggingFace token in this sandbox, so this script builds
# everything an operator needs to publish in one step: the model files
# (copied from the export dir, verified against MANIFEST.sha256), the
# Apache-2.0 licence, the README that doubles as the model card, and the
# exact upload commands printed at the end.
#
# Usage (from benchmarks/decide-model):
#   ./scripts/publish_bundle.sh artifact/xerj-decide-v1 dist/xerj-decide-v1
set -eu
src="${1:-artifact/xerj-decide-v1}"
dst="${2:-dist/xerj-decide-v1}"

[ -f "$src/model.safetensors" ] || { echo "missing $src/model.safetensors — run export first" >&2; exit 1; }
mkdir -p "$dst"

# 1. the model files, exactly as the loader wants them, digest-checked
for f in config.json tokenizer.json model.safetensors VERSION.json MANIFEST.sha256; do
    cp "$src/$f" "$dst/$f"
done
(cd "$dst" && sha256sum -c MANIFEST.sha256)

# 2. the licence: the weights are Apache-2.0 from xerj-org
repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
if [ -f "$repo_root/LICENSE" ]; then
    cp "$repo_root/LICENSE" "$dst/LICENSE"
else
    curl -fsSL https://www.apache.org/licenses/LICENSE-2.0.txt -o "$dst/LICENSE"
fi

# 3. the model card: the eval card is the single source of truth, so ship it
#    with the bundle rather than maintaining two copies of the numbers.
cp "$repo_root/docs/DECIDE_MODEL.md" "$dst/README.md"

echo
echo "bundle ready: $dst"
ls -la "$dst"
echo
cat <<'EOF'
To publish (operator; needs HF credentials this sandbox does not have):

  # option A: huggingface-cli (pip install -U "huggingface_hub[cli]")
  huggingface-cli login --token "$HF_TOKEN"
  huggingface-cli repo create xerj-decide-v1 --type model -y --organization xerj-org
  huggingface-cli upload xerj-org/xerj-decide-v1 <bundle-dir>/* . --repo-type model

  # option B: plain HTTP against the Hub API
  curl -X POST "https://huggingface.co/api/repos/create" \
       -H "Authorization: Bearer $HF_TOKEN" \
       -H "content-type: application/json" \
       -d '{"type":"model","name":"xerj-decide-v1","organization":"xerj-org","license":"apache-2.0"}'
  for f in config.json tokenizer.json model.safetensors VERSION.json MANIFEST.sha256 LICENSE README.md; do
    curl -X PUT "https://huggingface.co/xerj-org/xerj-decide-v1/resolve/main/$f" \
         -H "Authorization: Bearer $HF_TOKEN" \
         -H "content-type: application/octet-stream" \
         --data-binary "@<bundle-dir>/$f"
  done

Then a node runs the tier against a local copy of the published files (the
loader takes a directory — fetching a URL would be a loader change, tracked
as a follow-up, not something this harness does):

  curl -fsSL -o model.safetensors https://huggingface.co/xerj-org/xerj-decide-v1/resolve/main/model.safetensors
  # ... likewise config.json + tokenizer.json, then:
  sha256sum -c MANIFEST.sha256
  xerj --decide-mode local --decide-model-dir ./xerj-decide-v1

Publication is PENDING until an operator with credentials runs the steps
above; no published URL exists yet and none should be claimed.
EOF
