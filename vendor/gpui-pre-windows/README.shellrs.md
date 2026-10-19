# ShellRS Windows rendering patch

Source: crates.io `gpui-pre-windows` **0.3.2**, the `gpui_windows` snapshot
of Zed revision `801c087af22dd189dc1aa49e2f370b4f04190b19`. The original
Apache-2.0 license is in `LICENSE-APACHE`. All other GPUI crates remain at
their existing versions. This directory is selected by `[patch.crates-io]`
in the repository's Cargo.toml; do not edit the global Cargo registry cache.

Changes are limited to `src/gpui_windows.rs`, `src/directx_renderer.rs`,
the new `src/frame_damage.rs` and `src/directx_renderer_tests.rs`:

- Keep a coherent offscreen BGRA render target, independent of rotating DXGI
  back buffers. This costs an extra width × height × 4 bytes per window.
- Compare ordered scene primitives field by field. No hash collisions or reads
  of struct padding. Include both removed and added bounds, clipping,
  transformed sprites, antialiasing margins, and shadow blur extents.
- Compare spatial tiles (128 physical pixels) to avoid treating unrelated
  changes as one large rectangle. Changed bounds within a tile remain exact;
  tiles are an index, not a minimum repaint size. Merge nearby regions with
  at most 20% area inflation, cap submission at 16 rectangles and fall back
  to a combined/full frame for broad damage (70% coverage).
- Clear and redraw each dirty rectangle, including every primitive overlapping
  it in original draw order. Unchanged scenes skip presentation. Filtering
  reuses the retained primitive list instead of cloning the whole scene again.
- Copy only those rectangles from the retained target into the back buffer and
  call `Present1` with the matching rectangle list. Do not assume that the
  rotating back buffer contains the preceding frame's pixels.
- Resize, device recovery, changed clear colors and unknown surfaces trigger
  a full frame. Polychrome images are conservatively dirtied each frame because
  their atlas data can change without a different tile ID.
- Missing D3D11.1 rectangle clear support uses full frames. A partial-rendering
  error disables the optimization for that renderer and retries a full frame.
- `SHELLRS_DISABLE_PARTIAL_RENDER=1` selects full frames for A/B comparison.
  It still uses the retained target, so this is not the exact original binary.

Validation from the repository root on Windows:

```powershell
cargo test --locked -p gpui-pre-windows partial_warp -- --nocapture
```

The test creates hidden windows and actual WARP software devices. It compares
read-back pixels against full redraws across cursor movement, erasure, paths,
glyph transforms, translucent overlays and shadows, resize and device recovery.
It also covers simultaneous distant changes at 4K and executes the `Present1`
path. It does not measure the remote RDP
encoder, network, or client decode latency.

When updating GPUI, rebase these changes onto the corresponding Windows crate
and rerun the WARP tests. Do not replace this directory with a newer crate while
leaving the rest of the 0.3.2 stack unchanged.
