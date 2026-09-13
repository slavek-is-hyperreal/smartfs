# IDEA: SmartFS Multimedia Direct-to-VRAM Pipeline & Vulkan Compute Decoding

**Status:** Conceptual / Brainstorm  
**Author:** slavekm & Antigravity  
**Date:** 2026-09-13  
**Target Subsystems:** `smartfs-store`, `smartfs-fuse`, GPU compute layer (ADR-55)

---

## 1. Context & Motivation

SmartFS is architectured around immutable, content-addressed storage (Root Invariant #1: `content_hash = SHA-256(raw_bytes)` before compression; Root Invariant #2: Copy-on-Write immutability). Every version of every asset is backed by an immutable blob.

Modern multimedia workflows—such as professional video editing (NLE), color grading, visual effects (VFX), and multi-stream timeline scrubbing (4K, 6K, 8K)—are severely throttled by traditional operating system I/O architectures:
1. **The Double-Hop I/O Tax:** Disk (NVMe) → Host Kernel Buffer Cache → Userspace Host RAM → PCIe bus → GPU VRAM.
2. **ASIC Video Decoder Limitations:** Hardware fixed-function decoders (NVDEC, AMD VCN, Intel QuickSync) only support consumer distribution formats (H.264, HEVC, VP9, AV1). They do **not** support professional acquisition and mastering codecs:
   - Apple ProRes (422 HQ, 4444 XQ, RAW)
   - Avid DNxHR / DNxHD
   - CineForm
   - OpenEXR multi-layer image sequences
   - REDCODE RAW / CinemaDNG
3. **CPU Decoding Bottleneck:** As a result, professional editing suites force the CPU to decode tens of gigabytes per second of raw wavelet/DCT video frames, copy uncompressed RGB/YUV buffers to host RAM, and then upload them to GPU textures for rendering.

---

## 2. Core Proposal

Transform SmartFS into an ultra-high-performance filesystem for multimedia by introducing a **Direct-to-VRAM loading path** coupled with an **in-VRAM Vulkan compute decoding pipeline**:

```
 ┌────────────────────────────────────────────────────────────┐
 │                   NVMe SSD (SmartFS Blobs)                 │
 └─────────────────────────────┬──────────────────────────────┘
                               │
            Direct DMA via     │ (bypasses CPU RAM completely)
      Linux dma-buf / p2pmem   │
                               ▼
 ┌────────────────────────────────────────────────────────────┐
 │                     GPU VRAM BUFFER                        │
 │            (Compressed / Raw Bitstream Blob)               │
 └─────────────────────────────┬──────────────────────────────┘
                               │
            Vulkan Compute     │ (Parallel Wavelet / DCT / IDCT
               Shaders         │  Huffman / Run-Length decoders)
                               ▼
 ┌────────────────────────────────────────────────────────────┐
 │                 Decoded RGBA/YUV Textures                  │
 │              (Zero CPU-GPU memory transfers)               │
 └────────────────────────────────────────────────────────────┘
```

---

## 3. Key Architectural Pillars

### 3.1. Direct-to-VRAM I/O (Bypassing Host RAM)
- **Mechanism:** Leverage Linux peer-to-peer DMA (`p2pmem`), `dma-buf`, and Vulkan extensions (`VK_EXT_external_memory_dma_buf` / `VK_KHR_external_memory_fd`).
- Similar to Microsoft DirectStorage and NVIDIA GPUDirect Storage (GDS), NVMe storage controllers stream data directly into PCIe BAR memory mapped into GPU physical memory addresses.
- The host CPU never allocates intermediate buffers or manages page-cache copies for video streams.

### 3.2. Vulkan Compute Shaders for Non-ASIC Codecs
- For codecs without fixed-function silicon decoders (ProRes, DNxHR, CineForm, EXR, RAW), implement the decoding pipeline directly as **Vulkan Compute Kernels (SPIR-V)**:
  - **Bitstream Parsing & Huffman/VLC Decoding:** Parallel prefix sum (scan) and entropy decoding on GPU compute warps/subgroups.
  - **Inverse Quantization & IDCT / Wavelet Synthesis:** Massively parallel FP32/FP16 SIMD execution on GPU compute units.
  - **Color Space Transform & Debayering:** Bayer pattern demosaicing (for RAW footage), YUV-to-RGB matrix conversions, and EOTF/color grading directly into Vulkan textures.
- **Hardware-Agnostic Advantage (ADR-55):** By standardizing on Vulkan 1.3 compute shaders instead of proprietary CUDA (NVDEC/nvJPEG), the pipeline runs uniformly across AMD Radeon, Intel Arc, and NVIDIA GeForce/RTX GPUs on Linux Mint and other distributions.

### 3.3. GPU-Side Content-Addressed Blob Cache
- Root Invariant #1 guarantees that SmartFS blobs are immutable and uniquely identified by `content_hash`.
- A GPU-resident cache manager can retain decoded frame textures in VRAM, indexed by `(content_hash, frame_offset)`.
- Scrubbing back and forth across a timeline produces **zero I/O and zero re-decoding** if the frames are already resident in GPU memory.

---

## 4. Integration with SmartFS Architecture

1. **Plugin System:**
   - Define a `multimedia` / `video` plugin type in `smartfs-schema`.
   - The plugin indexes metadata: container headers, tracks, timecode, GOP structures, frame byte offsets, and keyframe tables into `special_data` JSONB or dedicated vector indices.
2. **FUSE vs. Direct API:**
   - Standard NLE applications access the files via POSIX FUSE as standard `.mov`, `.mxf`, or image sequence files.
   - Optimized multimedia applications (or a SmartFS Multimedia SDK) can request direct VRAM handles through an ioctl or IPC protocol, retrieving raw memory descriptors (`dma-buf` file descriptors) without reading through FUSE read loops.
3. **Consolidation & AI Metadata:**
   - Frames decoded in VRAM can be fed directly to on-GPU visual embedding models (e.g. CLIP / Qwen3-VL in post-MVP) without copying back to CPU, enabling real-time semantic search over video frames and visual concept centroids (`smartfs-semantic`).

---

## 5. Feasibility & Industry Precedents

- **Is this an exotic problem?**
  No, it addresses a major industry pain point. High-end visual effects and virtual production pipelines (e.g. DaVinci Resolve, Unreal Engine, Blender, Foundry Nuke) struggle with I/O throughput on multi-stream 8K footage.
- **Precedents:**
  - RED Digital Cinema previously developed RED ROCKET and GPU-accelerated debayering.
  - Apple Metal ProRes hardware & software decoding frameworks.
  - Microsoft DirectStorage 1.2 (GPU decompression of GDeflate/Zstandard).
  - NVIDIA GPUDirect Storage (GDS) with nvImageCodec.
- SmartFS's unique advantage is combining **content-addressed immutability** with **direct hardware acceleration**, making caching and multi-user deduplication mathematically deterministic.
