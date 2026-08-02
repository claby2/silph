# Vendored assets

## uPlot 1.6.32

- Source: https://github.com/leeoniya/uPlot/tree/1.6.32/dist
- License: MIT (see UPLOT-LICENSE)
- sha256:
  - `uplot.iife.min.js`: `19c8d4c6ad88929a79f4ae49d6f7161566dfd0ba3d15cc495e974f787eb78f1f`
  - `uplot.min.css`: `df630c6a8d6f8eeaff264b50f73ce5b114f646ffd9a0bb74f049b0a00135fa04`

Committed to the repo (rather than fetched at build time) so both `cargo build`
and the nix build stay pure and offline.
