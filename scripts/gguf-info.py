#!/usr/bin/env python3
"""gguf-info.py — read a GGUF file's own header: architecture, tensor sizes, VRAM budget.

ADR-63 section 1e sizes the Vulkan offload from per-layer bytes and KV-cache
bytes per token. Those numbers came from this script reading the actual weights
files, not from a model card — if the weights are ever replaced, rerun it rather
than copying the table out of the ADR.

    scripts/gguf-info.py <file.gguf> [--vram MiB] [--ctx TOKENS]

With --vram it also prints how many of the model's layers fit on a card that
size, which is the question that decides whether a small GPU can accelerate at
all (llama.cpp's -ngl N offloads N layers and leaves the rest on the CPU).

See: docs/adr/ADR-63-embedding-model-placement.md
"""
import argparse
import collections
import struct
import sys

# GGUF metadata value types.
(T_U8, T_I8, T_U16, T_I16, T_U32, T_I32, T_F32, T_BOOL, T_STR, T_ARR,
 T_U64, T_I64, T_F64) = range(13)
SCALAR = {T_U8: '<B', T_I8: '<b', T_U16: '<H', T_I16: '<h', T_U32: '<I',
          T_I32: '<i', T_F32: '<f', T_BOOL: '<?', T_U64: '<Q', T_I64: '<q',
          T_F64: '<d'}

# ggml tensor type -> (elements per block, bytes per block). Only the types that
# actually appear in the models this project ships; an unknown id is an error
# rather than a guess, because a wrong size here would silently corrupt the
# VRAM budget that decides the offload.
GGML_TYPE = {
    0: (1, 4, 'F32'), 1: (1, 2, 'F16'), 8: (32, 34, 'Q8_0'),
    2: (32, 18, 'Q4_0'), 3: (32, 20, 'Q4_1'), 12: (256, 144, 'Q4_K'),
    13: (256, 176, 'Q5_K'), 14: (256, 210, 'Q6_K'), 30: (1, 2, 'BF16'),
}

# Metadata keys too large to be worth printing; they are data, not description.
SKIP_PREFIX = ('tokenizer.ggml.tokens', 'tokenizer.ggml.scores',
               'tokenizer.ggml.token_type', 'tokenizer.ggml.merges',
               'tokenizer.chat_template')


class Reader:
    def __init__(self, f):
        self.f = f

    def take(self, n):
        b = self.f.read(n)
        if len(b) != n:
            sys.exit(f"truncated GGUF: wanted {n} bytes, got {len(b)}")
        return b

    def scalar(self, t):
        if t == T_STR:
            n, = struct.unpack('<Q', self.take(8))
            return self.take(n).decode('utf-8', 'replace')
        fmt = SCALAR[t]
        return struct.unpack(fmt, self.take(struct.calcsize(fmt)))[0]

    def value(self, t):
        if t != T_ARR:
            return self.scalar(t)
        et, = struct.unpack('<I', self.take(4))
        n, = struct.unpack('<Q', self.take(8))
        # Every element must be consumed even when not displayed: the elements
        # are variable-width, so skipping by arithmetic is not possible and a
        # short read desynchronises everything after it.
        vals = [self.scalar(et) for _ in range(n)]
        return vals if n <= 16 else f'<array len={n}>'


def read_gguf(path):
    f = open(path, 'rb')
    rd = Reader(f)
    if rd.take(4) != b'GGUF':
        sys.exit(f"{path}: not a GGUF file")
    version, = struct.unpack('<I', rd.take(4))
    n_tensors, = struct.unpack('<Q', rd.take(8))
    n_kv, = struct.unpack('<Q', rd.take(8))

    kv = {}
    for _ in range(n_kv):
        key = rd.scalar(T_STR)
        t, = struct.unpack('<I', rd.take(4))
        kv[key] = rd.value(t)

    tensors = []
    for _ in range(n_tensors):
        name = rd.scalar(T_STR)
        n_dims, = struct.unpack('<I', rd.take(4))
        dims = [struct.unpack('<Q', rd.take(8))[0] for _ in range(n_dims)]
        tt, = struct.unpack('<I', rd.take(4))
        struct.unpack('<Q', rd.take(8))  # offset, unused here
        if tt not in GGML_TYPE:
            sys.exit(f"{path}: unknown ggml tensor type {tt} for {name}")
        block_ne, block_bytes, tname = GGML_TYPE[tt]
        ne = 1
        for d in dims:
            ne *= d
        tensors.append((name, dims, tname, ne // block_ne * block_bytes))
    f.close()
    return version, kv, tensors


MIB = 1024 * 1024


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('file')
    ap.add_argument('--vram', type=float, default=None,
                    help='free VRAM in MiB, to compute the offload budget')
    ap.add_argument('--ctx', type=int, default=1024,
                    help='context length in tokens for the KV-cache estimate')
    ap.add_argument('--headroom', type=float, default=128.0,
                    help='MiB reserved for compute/scratch buffers')
    args = ap.parse_args()

    version, kv, tensors = read_gguf(args.file)
    arch = kv.get('general.architecture', '?')
    print(f"{args.file}")
    print(f"  gguf v{version}  arch={arch}  tensors={len(tensors)}")
    for k in sorted(kv):
        if k.startswith(SKIP_PREFIX):
            continue
        print(f"  {k} = {kv[k]}")

    groups = collections.OrderedDict()
    for name, _dims, tname, nbytes in tensors:
        g = 'blk' if name.startswith('blk.') else name.split('.')[0]
        e = groups.setdefault(g, [0, 0, set()])
        e[0] += nbytes
        e[1] += 1
        e[2].add(tname)

    print("  --- bytes by tensor group ---")
    for g, (b, c, types) in groups.items():
        print(f"  {g:14} {b / MIB:9.1f} MiB  tensors={c:3}  {','.join(sorted(types))}")

    n_layer = kv.get(f'{arch}.block_count')
    total = sum(t[3] for t in tensors)
    layer_mib = groups['blk'][0] / n_layer / MIB if n_layer and 'blk' in groups else None
    if layer_mib:
        print(f"  per layer ({n_layer}) {layer_mib:9.1f} MiB")
    print(f"  TOTAL          {total / MIB:9.1f} MiB")

    # KV cache: K and V, per KV head, per layer, at f16.
    n_kv_head = kv.get(f'{arch}.attention.head_count_kv')
    k_len = kv.get(f'{arch}.attention.key_length')
    v_len = kv.get(f'{arch}.attention.value_length')
    if None in (n_layer, n_kv_head, k_len, v_len):
        return
    kv_per_token = n_layer * n_kv_head * (k_len + v_len) * 2  # bytes, f16
    print(f"  KV cache       {kv_per_token / 1024:9.1f} KiB per token"
          f"  ({kv_per_token * args.ctx / MIB:.1f} MiB at ctx={args.ctx})")

    if args.vram is None or not layer_mib:
        return
    usable = args.vram - kv_per_token * args.ctx / MIB - args.headroom
    fits = max(0, min(n_layer, int(usable // layer_mib)))
    print(f"  --- offload budget: {args.vram:.0f} MiB free VRAM, ctx={args.ctx},"
          f" {args.headroom:.0f} MiB headroom ---")
    print(f"  layers on GPU  {fits} of {n_layer}"
          f"  ({fits * layer_mib:.1f} MiB of weights)")
    if fits == n_layer:
        spare = usable - n_layer * layer_mib
        print(f"  whole model fits, {spare:.0f} MiB to spare")
    elif fits == 0:
        print("  nothing fits: this card cannot help at this context length")


if __name__ == '__main__':
    main()
