use std::f32::consts::PI;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use nih_plug::prelude::*;
use nih_plug_egui::egui::{
	self, Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2,
};
use nih_plug_egui::{EguiState, create_egui_editor};

use crate::IkupeserParams;
use crate::dsp::group_delay_ms;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 420;

const BACKGROUND: Color32 = Color32::from_rgb(0x15, 0x16, 0x1c);
const PANEL: Color32 = Color32::from_rgb(0x1e, 0x20, 0x29);
const GRID: Color32 = Color32::from_rgb(0x2c, 0x2f, 0x3b);
const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdb, 0xe6);
const TEXT_DIM: Color32 = Color32::from_rgb(0x7c, 0x81, 0x94);
const ACCENT: Color32 = Color32::from_rgb(0x5c, 0xe1, 0xe6);
const ACCENT_FILL: Color32 = Color32::from_rgba_premultiplied(0x17, 0x38, 0x3a, 0x40);

const MIN_FREQUENCY: f32 = 20.0;
const MAX_FREQUENCY: f32 = 20_000.0;
const PLOT_POINTS: usize = 256;

/// Vertical drag distance in points that sweeps a knob across its full range.
const KNOB_DRAG_RANGE: f32 = 200.0;
/// Drag sensitivity divisor applied while Shift is held.
const FINE_DRAG_DIVISOR: f32 = 10.0;

pub fn default_state() -> Arc<EguiState> {
	EguiState::from_size(WIDTH, HEIGHT)
}

pub fn create(params: Arc<IkupeserParams>, sample_rate: Arc<AtomicF32>) -> Option<Box<dyn Editor>> {
	create_egui_editor(
		params.editor_state.clone(),
		(),
		|_, _| {},
		move |ctx, setter, _| {
			egui::CentralPanel::default()
				.frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::same(16)))
				.show(ctx, |ui| {
					header(ui);
					ui.add_space(12.0);
					delay_plot(ui, &params, setter, sample_rate.load(Ordering::Relaxed));
					ui.add_space(16.0);
					ui.horizontal(|ui| {
						let spacing = (ui.available_width() - 4.0 * KNOB_SIZE.x) / 3.0;
						ui.spacing_mut().item_spacing.x = spacing.max(0.0);
						knob(ui, &params.amount, setter);
						knob(ui, &params.frequency, setter);
						knob(ui, &params.pinch, setter);
						knob(ui, &params.mix, setter);
					});
				});
		},
	)
}

fn header(ui: &mut Ui) {
	ui.horizontal(|ui| {
		ui.label(
			egui::RichText::new("IKUPESER")
				.font(FontId::proportional(22.0))
				.color(TEXT)
				.strong(),
		);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			ui.label(egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).color(TEXT_DIM));
		});
	});
}

fn frequency_to_x(rect: Rect, frequency: f32) -> f32 {
	let t = (frequency / MIN_FREQUENCY).ln() / (MAX_FREQUENCY / MIN_FREQUENCY).ln();
	rect.left() + t * rect.width()
}

fn x_to_frequency(rect: Rect, x: f32) -> f32 {
	let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
	MIN_FREQUENCY * (MAX_FREQUENCY / MIN_FREQUENCY).powf(t)
}

/// Smallest value of the form `{1, 2, 5} * 10^n` that is greater than or equal to `value`. The
/// plot's vertical range is rounded to this so that the axis label stays readable.
fn nice_ceil(value: f32) -> f32 {
	let magnitude = 10f32.powf(value.max(1.0e-3).log10().floor());
	[1.0, 2.0, 5.0, 10.0]
		.into_iter()
		.map(|m| m * magnitude)
		.find(|&v| v >= value)
		.unwrap_or(10.0 * magnitude)
}

/// Draws the group delay curve. Horizontal dragging on the plot sets the `Frequency` parameter.
fn delay_plot(ui: &mut Ui, params: &IkupeserParams, setter: &ParamSetter, sample_rate: f32) {
	let size = Vec2::new(ui.available_width(), 220.0);
	let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
	let painter = ui.painter_at(rect);
	painter.rect_filled(rect, CornerRadius::same(6), PANEL);

	let plot = rect.shrink2(Vec2::new(8.0, 18.0));

	for decade in [100.0, 1_000.0, 10_000.0] {
		for step in 1..10 {
			let frequency = decade / 10.0 * step as f32;
			if frequency < MIN_FREQUENCY {
				continue;
			}
			let x = frequency_to_x(plot, frequency);
			let stroke = Stroke::new(if step == 1 { 1.0_f32 } else { 0.5 }, GRID);
			painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], stroke);
		}
		let label = if decade >= 1_000.0 {
			format!("{}k", decade / 1_000.0)
		} else {
			format!("{decade}")
		};
		painter.text(
			Pos2::new(frequency_to_x(plot, decade) + 3.0, rect.bottom() - 3.0),
			Align2::LEFT_BOTTOM,
			label,
			FontId::proportional(10.0),
			TEXT_DIM,
		);
	}

	let center = params.frequency.value();
	let q = params.pinch.value();
	let amount = params.amount.value();
	let mix = params.mix.value();

	let delays: Vec<(f32, f32)> = (0..PLOT_POINTS)
		.map(|i| {
			let x = plot.left() + plot.width() * i as f32 / (PLOT_POINTS - 1) as f32;
			(
				x,
				group_delay_ms(x_to_frequency(plot, x), center, q, amount, sample_rate),
			)
		})
		.collect();
	let max_delay = nice_ceil(delays.iter().map(|&(_, d)| d).fold(0.0, f32::max));
	let to_y = |delay: f32| plot.bottom() - (delay / max_delay).min(1.0) * plot.height();

	let curve_color = ACCENT.gamma_multiply(0.35 + 0.65 * mix);
	let mut mesh = Mesh::default();
	for (i, &(x, delay)) in delays.iter().enumerate() {
		mesh.colored_vertex(Pos2::new(x, to_y(delay)), ACCENT_FILL);
		mesh.colored_vertex(Pos2::new(x, plot.bottom()), Color32::TRANSPARENT);
		if i > 0 {
			let base = (2 * i) as u32;
			mesh.add_triangle(base - 2, base - 1, base);
			mesh.add_triangle(base - 1, base, base + 1);
		}
	}
	painter.add(Shape::mesh(mesh));
	painter.add(Shape::line(
		delays.iter().map(|&(x, delay)| Pos2::new(x, to_y(delay))).collect(),
		Stroke::new(2.0_f32, curve_color),
	));

	let center_x = frequency_to_x(plot, center);
	painter.line_segment(
		[Pos2::new(center_x, plot.top()), Pos2::new(center_x, plot.bottom())],
		Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.5)),
	);

	painter.text(
		rect.left_top() + Vec2::new(8.0, 4.0),
		Align2::LEFT_TOP,
		format!("{max_delay} ms"),
		FontId::proportional(10.0),
		TEXT_DIM,
	);
	painter.text(
		rect.right_top() + Vec2::new(-8.0, 4.0),
		Align2::RIGHT_TOP,
		"group delay",
		FontId::proportional(10.0),
		TEXT_DIM,
	);

	if response.drag_started() || response.clicked() {
		setter.begin_set_parameter(&params.frequency);
	}
	if let Some(pointer) = response.interact_pointer_pos() {
		if response.dragged() || response.clicked() {
			setter.set_parameter(&params.frequency, x_to_frequency(plot, pointer.x));
		}
	}
	if response.drag_stopped() || response.clicked() {
		setter.end_set_parameter(&params.frequency);
	}
}

const KNOB_SIZE: Vec2 = Vec2::new(96.0, 120.0);
const KNOB_RADIUS: f32 = 30.0;
/// The knob arc spans 270 degrees, starting at the lower left.
const KNOB_START_ANGLE: f32 = 0.75 * PI;
const KNOB_SWEEP: f32 = 1.5 * PI;

/// A rotary control bound to `param`. Vertical dragging changes the normalized value, Shift enables
/// fine adjustment, and double-clicking restores the default value.
fn knob<P: Param>(ui: &mut Ui, param: &P, setter: &ParamSetter) -> Response {
	let (rect, response) = ui.allocate_exact_size(KNOB_SIZE, Sense::click_and_drag());

	if response.drag_started() {
		setter.begin_set_parameter(param);
	}
	if response.dragged() {
		let mut delta = -response.drag_delta().y / KNOB_DRAG_RANGE;
		if ui.input(|i| i.modifiers.shift) {
			delta /= FINE_DRAG_DIVISOR;
		}
		let value = (param.unmodulated_normalized_value() + delta).clamp(0.0, 1.0);
		setter.set_parameter_normalized(param, value);
	}
	if response.drag_stopped() {
		setter.end_set_parameter(param);
	}
	if response.double_clicked() {
		setter.begin_set_parameter(param);
		setter.set_parameter_normalized(param, param.default_normalized_value());
		setter.end_set_parameter(param);
	}

	let painter = ui.painter_at(rect);
	let center = Pos2::new(rect.center().x, rect.top() + 8.0 + KNOB_RADIUS);
	let normalized = param.unmodulated_normalized_value();
	let arc = |from: f32, to: f32, radius: f32| -> Vec<Pos2> {
		let steps = 48;
		(0..=steps)
			.map(|i| {
				let angle = KNOB_START_ANGLE + KNOB_SWEEP * (from + (to - from) * i as f32 / steps as f32);
				center + radius * Vec2::angled(angle)
			})
			.collect()
	};

	let highlighted = response.hovered() || response.dragged();
	painter.circle_filled(center, KNOB_RADIUS - 6.0, if highlighted { GRID } else { PANEL });
	painter.add(Shape::line(arc(0.0, 1.0, KNOB_RADIUS), Stroke::new(4.0_f32, GRID)));
	painter.add(Shape::line(
		arc(0.0, normalized, KNOB_RADIUS),
		Stroke::new(4.0_f32, ACCENT),
	));

	let pointer_dir = Vec2::angled(KNOB_START_ANGLE + KNOB_SWEEP * normalized);
	painter.line_segment(
		[center + pointer_dir * 8.0, center + pointer_dir * (KNOB_RADIUS - 10.0)],
		Stroke::new(2.0_f32, TEXT),
	);

	painter.text(
		Pos2::new(rect.center().x, center.y + KNOB_RADIUS + 12.0),
		Align2::CENTER_CENTER,
		param.name().to_uppercase(),
		FontId::proportional(12.0),
		TEXT_DIM,
	);
	painter.text(
		Pos2::new(rect.center().x, center.y + KNOB_RADIUS + 30.0),
		Align2::CENTER_CENTER,
		param.normalized_value_to_string(normalized, true),
		FontId::proportional(14.0),
		TEXT,
	);

	response
}
