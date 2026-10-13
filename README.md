# Ikuperser

A phase dispersion effect in the spirit of Kilohearts Disperser, written in Rust with
[nih-plug](https://github.com/robbert-vdh/nih-plug). The signal passes through a cascade of up to 200
identical second-order allpass filters, which delays frequencies around the center frequency and
turns transients into chirps.

Formats: VST3 and CLAP.

## Parameters

| Parameter | Range          | Description                                                                 |
| --------- | -------------- | --------------------------------------------------------------------------- |
| Amount    | 0 – 200        | Number of cascaded allpass stages.                                          |
| Frequency | 20 Hz – 20 kHz | Center frequency of every stage.                                            |
| Pinch     | 0.1 – 20       | Q of every stage. Higher values concentrate the delay into a narrower band. |
| Mix       | 0 – 100 %      | Dry/wet balance.                                                            |

The plot in the editor shows the group delay of the current settings. Dragging horizontally on the
plot sets the frequency. Knobs are dragged vertically; hold Shift for fine adjustment and
double-click to reset.

## Building

```bash
cargo xtask bundle ikuperser --release
```

The bundles are written to `target/bundled/` (`Ikuperser.vst3`, `Ikuperser.clap`).
On Windows, copy `Ikuperser.vst3` to `C:\Program Files\Common Files\VST3\`.

## Testing

```bash
cargo test --release
```
