use std::f32::consts::PI;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use nih_plug::prelude::*;
use nih_plug_egui::egui::{
	self, Align2, Color32, CornerRadius, FontId, Mesh, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, Ui,
	Vec2,
};
use nih_plug_egui::{EguiState, create_egui_editor};

use crate::IkuperserParams;
use crate::dsp::{MAX_STAGES, group_delay_ms};

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
const PLOT_POINTS: usize = 1024;

/// Vertical drag distance in points that sweeps a knob across its full range.
const KNOB_DRAG_RANGE: f32 = 200.0;
/// Drag sensitivity divisor applied while Shift is held.
const FINE_DRAG_DIVISOR: f32 = 10.0;
const HOST_VALUE_TIMEOUT: f64 = 0.5;

#[derive(Default)]
struct EditorState {
	amount: ControlState,
	frequency: ControlState,
	pinch: ControlState,
	mix: ControlState,
}

struct DragState {
	normalized: f32,
	pointer_y: f32,
}

impl DragState {
	fn move_to(&mut self, pointer_y: f32, fine: bool) -> f32 {
		let divisor = if fine { FINE_DRAG_DIVISOR } else { 1.0 };
		self.normalized =
			(self.normalized + (self.pointer_y - pointer_y) / (KNOB_DRAG_RANGE * divisor)).clamp(0.0, 1.0);
		self.pointer_y = pointer_y;
		self.normalized
	}
}

struct PendingValue {
	normalized: f32,
	since: f64,
}

#[derive(Default)]
struct ControlState {
	// Retain sub-step movement independently of rounded or delayed host values.
	drag: Option<DragState>,
	pending: Option<PendingValue>,
}

impl ControlState {
	fn normalized(&mut self, param: &FloatParam, now: f64) -> f32 {
		if let Some(drag) = &self.drag {
			return param.preview_normalized(param.preview_plain(drag.normalized));
		}

		let host_value = param.unmodulated_normalized_value();
		if let Some(pending) = &self.pending {
			// Keep the submitted value visible until acknowledgement, with a timeout for host rejection.
			if (host_value - pending.normalized).abs() > 1.0e-6 && now - pending.since < HOST_VALUE_TIMEOUT {
				return pending.normalized;
			}
		}
		self.pending = None;
		host_value
	}

	fn plain_value(&mut self, param: &FloatParam, now: f64) -> f32 {
		let normalized = self.normalized(param, now);
		if self.drag.is_some() || self.pending.is_some() {
			param.preview_plain(normalized)
		} else {
			param.value()
		}
	}

	fn begin_drag(&mut self, param: &FloatParam, pointer_y: f32, now: f64) {
		self.drag = Some(DragState {
			normalized: self.normalized(param, now),
			pointer_y,
		});
	}

	fn set_normalized(&mut self, param: &FloatParam, setter: &ParamSetter, normalized: f32, now: f64) {
		let plain = param.preview_plain(normalized);
		let normalized = param.preview_normalized(plain);
		let previous = self
			.pending
			.as_ref()
			.map_or_else(|| param.unmodulated_normalized_value(), |pending| pending.normalized);
		if normalized != previous {
			setter.set_parameter(param, plain);
			self.pending = Some(PendingValue { normalized, since: now });
		}
	}

	fn end_drag(&mut self, now: f64) {
		self.drag = None;
		if let Some(pending) = &mut self.pending {
			pending.since = now;
		}
	}
}

pub fn default_state() -> Arc<EguiState> {
	EguiState::from_size(WIDTH, HEIGHT)
}

pub fn create(params: Arc<IkuperserParams>, sample_rate: Arc<AtomicF32>) -> Option<Box<dyn Editor>> {
	create_egui_editor(
		params.editor_state.clone(),
		EditorState::default(),
		|_, _| {},
		move |ctx, setter, state| {
			egui::CentralPanel::default()
				.frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::same(16)))
				.show(ctx, |ui| {
					header(ui);
					ui.add_space(12.0);
					let (plot_rect, plot_response) =
						ui.allocate_exact_size(Vec2::new(ui.available_width(), 220.0), Sense::click_and_drag());
					plot_input(ui, &params.frequency, setter, &mut state.frequency, &plot_response);
					ui.add_space(16.0);
					ui.horizontal(|ui| {
						let spacing = (ui.available_width() - 4.0 * KNOB_SIZE.x) / 3.0;
						ui.spacing_mut().item_spacing.x = spacing.max(0.0);
						knob(ui, &params.amount, setter, &mut state.amount);
						knob(ui, &params.frequency, setter, &mut state.frequency);
						knob(ui, &params.pinch, setter, &mut state.pinch);
						knob(ui, &params.mix, setter, &mut state.mix);
					});
					// Paint after input processing so the plot and knobs use the same submitted values.
					delay_plot(ui, plot_rect, &params, state, sample_rate.load(Ordering::Relaxed));
				});
		},
	)
}

fn header(ui: &mut Ui) {
	ui.horizontal(|ui| {
		ui.label(
			egui::RichText::new("IKUPERSER")
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

fn delay_curve(plot: Rect, center: f32, q: f32, amount: f32, sample_rate: f32) -> Vec<(f32, f32)> {
	let mut delays: Vec<(f32, f32)> = (0..PLOT_POINTS)
		.map(|i| {
			let x = plot.left() + plot.width() * i as f32 / (PLOT_POINTS - 1) as f32;
			(
				x,
				group_delay_ms(x_to_frequency(plot, x), center, q, amount, sample_rate),
			)
		})
		.collect();

	// Include the center independently of the grid so narrow peaks remain sampled while dragging.
	let center_x = frequency_to_x(plot, center).clamp(plot.left(), plot.right());
	let index = delays.partition_point(|&(x, _)| x < center_x);
	delays.insert(
		index,
		(
			center_x,
			group_delay_ms(x_to_frequency(plot, center_x), center, q, amount, sample_rate),
		),
	);
	delays
}

fn delay_plot_range(delays: &[(f32, f32)]) -> f32 {
	// Keep scaling continuous across label rounding boundaries and nonzero at Amount = 0.
	(delays.iter().map(|&(_, delay)| delay).fold(0.0, f32::max) * 1.1).max(1.0e-3)
}

fn delay_plot_data(plot: Rect, center: f32, q: f32, amount: f32, sample_rate: f32) -> (Vec<(f32, f32)>, f32) {
	// Use the full stage count as the axis reference so Amount changes affect curve height.
	let mut delays = delay_curve(plot, center, q, MAX_STAGES as f32, sample_rate);
	let max_delay = delay_plot_range(&delays);
	for (_, delay) in &mut delays {
		*delay *= amount / MAX_STAGES as f32;
	}
	(delays, max_delay)
}

fn plot_input(ui: &Ui, param: &FloatParam, setter: &ParamSetter, state: &mut ControlState, response: &Response) {
	let now = ui.input(|i| i.time);
	let clicked = response.clicked_by(PointerButton::Primary);
	if response.drag_started_by(PointerButton::Primary) || clicked {
		state.begin_drag(param, 0.0, now);
		setter.begin_set_parameter(param);
	}
	if (response.dragged_by(PointerButton::Primary) || response.drag_stopped() || clicked)
		&& let (Some(pointer), Some(drag)) = (response.interact_pointer_pos(), &mut state.drag)
	{
		let plot = response.rect.shrink2(Vec2::new(8.0, 18.0));
		drag.normalized = param.preview_normalized(x_to_frequency(plot, pointer.x));
		let normalized = drag.normalized;
		state.set_normalized(param, setter, normalized, now);
	}
	if (response.drag_stopped() || clicked) && state.drag.is_some() {
		state.end_drag(now);
		setter.end_set_parameter(param);
	}
}

fn delay_plot(ui: &mut Ui, rect: Rect, params: &IkuperserParams, state: &mut EditorState, sample_rate: f32) {
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

	let now = ui.input(|i| i.time);
	let center = state.frequency.plain_value(&params.frequency, now);
	let q = state.pinch.plain_value(&params.pinch, now);
	let amount = state.amount.plain_value(&params.amount, now);
	let mix = state.mix.plain_value(&params.mix, now);

	let (delays, max_delay) = delay_plot_data(plot, center, q, amount, sample_rate);
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
		format!("{max_delay:.3} ms"),
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
}

const KNOB_SIZE: Vec2 = Vec2::new(96.0, 120.0);
const KNOB_RADIUS: f32 = 30.0;
/// The knob arc spans 270 degrees, starting at the lower left.
const KNOB_START_ANGLE: f32 = 0.75 * PI;
const KNOB_SWEEP: f32 = 1.5 * PI;

/// A rotary control bound to `param`. Vertical dragging changes the normalized value, Shift enables
/// fine adjustment, and double-clicking restores the default value.
fn knob(ui: &mut Ui, param: &FloatParam, setter: &ParamSetter, state: &mut ControlState) -> Response {
	let (rect, response) = ui.allocate_exact_size(KNOB_SIZE, Sense::click_and_drag());
	let now = ui.input(|i| i.time);

	if response.drag_started_by(PointerButton::Primary) {
		let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(rect.center());
		state.begin_drag(param, origin.y, now);
		setter.begin_set_parameter(param);
	}
	if (response.dragged_by(PointerButton::Primary) || response.drag_stopped())
		&& let (Some(pointer), Some(drag)) = (response.interact_pointer_pos(), &mut state.drag)
	{
		let normalized = drag.move_to(pointer.y, ui.input(|i| i.modifiers.shift));
		state.set_normalized(param, setter, normalized, now);
	}
	if response.drag_stopped() && state.drag.is_some() {
		state.end_drag(now);
		setter.end_set_parameter(param);
	}
	if response.double_clicked_by(PointerButton::Primary) {
		setter.begin_set_parameter(param);
		state.set_normalized(param, setter, param.default_normalized_value(), now);
		setter.end_set_parameter(param);
	}

	let painter = ui.painter_at(rect);
	let center = Pos2::new(rect.center().x, rect.top() + 8.0 + KNOB_RADIUS);
	let normalized = state.normalized(param, now);
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

#[cfg(test)]
mod tests {
	use std::sync::Mutex;

	use super::*;

	const SAMPLE_RATE: f32 = 48_000.0;

	#[derive(Clone, Copy, Debug)]
	enum HostEvent {
		Begin,
		Set(f32),
		End,
	}

	#[derive(Default)]
	struct DelayedHost {
		events: Mutex<Vec<HostEvent>>,
	}

	impl GuiContext for DelayedHost {
		fn plugin_api(&self) -> PluginApi {
			PluginApi::Vst3
		}

		fn request_resize(&self) -> bool {
			true
		}

		unsafe fn raw_begin_set_parameter(&self, _param: ParamPtr) {
			self.events.lock().unwrap().push(HostEvent::Begin);
		}

		unsafe fn raw_set_parameter_normalized(&self, _param: ParamPtr, normalized: f32) {
			self.events.lock().unwrap().push(HostEvent::Set(normalized));
		}

		unsafe fn raw_end_set_parameter(&self, _param: ParamPtr) {
			self.events.lock().unwrap().push(HostEvent::End);
		}

		fn get_state(&self) -> PluginState {
			unimplemented!()
		}

		fn set_state(&self, _state: PluginState) {
			unimplemented!()
		}
	}

	struct KnobHarness {
		ctx: egui::Context,
		host: DelayedHost,
		state: ControlState,
		param: FloatParam,
		now: f64,
	}

	impl KnobHarness {
		fn new(param: FloatParam) -> Self {
			let mut harness = Self {
				ctx: egui::Context::default(),
				host: DelayedHost::default(),
				state: ControlState::default(),
				param,
				now: 0.0,
			};
			harness.frame(80.0, None, false);
			harness
		}

		fn frame(&mut self, y: f32, button: Option<(PointerButton, bool)>, shift: bool) {
			let Self {
				ctx,
				host,
				state,
				param,
				now,
			} = self;
			*now += 1.0 / 60.0;
			let pos = Pos2::new(60.0, y);
			let modifiers = egui::Modifiers {
				shift,
				..Default::default()
			};
			let mut events = vec![egui::Event::PointerMoved(pos)];
			if let Some((button, pressed)) = button {
				events.push(egui::Event::PointerButton {
					pos,
					button,
					pressed,
					modifiers,
				});
			}
			let _ = ctx.run(
				egui::RawInput {
					screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 420.0))),
					time: Some(*now),
					events,
					modifiers,
					..Default::default()
				},
				|ctx| {
					egui::CentralPanel::default().show(ctx, |ui| {
						knob(ui, param, &ParamSetter::new(host), state);
					});
				},
			);
		}

		fn sent_values(&self) -> Vec<f32> {
			self.host
				.events
				.lock()
				.unwrap()
				.iter()
				.filter_map(|event| {
					if let HostEvent::Set(value) = event {
						Some(*value)
					} else {
						None
					}
				})
				.collect()
		}
	}

	fn continuous_param(value: f32) -> FloatParam {
		FloatParam::new("Test", value, FloatRange::Linear { min: 0.0, max: 1.0 })
	}

	#[test]
	fn drag_accumulates_without_host_feedback() {
		let mut gui = KnobHarness::new(continuous_param(0.5));
		gui.frame(80.0, Some((PointerButton::Primary, true)), false);
		for y in [70.0, 65.0, 60.0, 55.0, 50.0] {
			gui.frame(y, None, false);
		}
		assert_eq!(gui.param.value(), 0.5);
		assert!((gui.state.normalized(&gui.param, gui.now) - 0.65).abs() < 1.0e-6);
		assert!((gui.state.plain_value(&gui.param, gui.now) - 0.65).abs() < 1.0e-6);
		gui.frame(50.0, Some((PointerButton::Primary, false)), false);
		gui.frame(50.0, None, false);
		assert!((gui.state.normalized(&gui.param, gui.now) - 0.65).abs() < 1.0e-6);
		let values = gui.sent_values();
		assert_eq!(values.len(), 5);
		assert!((values[0] - 0.55).abs() < 1.0e-6);
		assert!((values[4] - 0.65).abs() < 1.0e-6);
		assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
		let events = gui.host.events.lock().unwrap();
		assert!(matches!(events.first(), Some(HostEvent::Begin)));
		assert!(matches!(events.last(), Some(HostEvent::End)));
		assert_eq!(events.iter().filter(|e| matches!(e, HostEvent::Begin)).count(), 1);
		assert_eq!(events.iter().filter(|e| matches!(e, HostEvent::End)).count(), 1);
	}

	#[test]
	fn release_applies_final_pointer_movement() {
		let mut gui = KnobHarness::new(continuous_param(0.5));
		gui.frame(80.0, Some((PointerButton::Primary, true)), false);
		gui.frame(70.0, None, false);
		gui.frame(50.0, Some((PointerButton::Primary, false)), false);
		assert!(gui.state.drag.is_none());
		assert!((gui.state.normalized(&gui.param, gui.now) - 0.65).abs() < 1.0e-6);
		assert!(matches!(gui.host.events.lock().unwrap().last(), Some(HostEvent::End)));
	}

	#[test]
	fn double_click_resets_with_balanced_gesture() {
		let mut gui = KnobHarness::new(continuous_param(0.5));
		gui.frame(80.0, Some((PointerButton::Primary, true)), false);
		gui.frame(50.0, None, false);
		gui.frame(50.0, Some((PointerButton::Primary, false)), false);
		for _ in 0..2 {
			gui.frame(80.0, Some((PointerButton::Primary, true)), false);
			gui.frame(80.0, Some((PointerButton::Primary, false)), false);
		}
		assert_eq!(gui.state.normalized(&gui.param, gui.now), 0.5);
		assert_eq!(gui.sent_values().last(), Some(&0.5));
		let events = gui.host.events.lock().unwrap();
		assert_eq!(events.iter().filter(|e| matches!(e, HostEvent::Begin)).count(), 2);
		assert_eq!(events.iter().filter(|e| matches!(e, HostEvent::End)).count(), 2);
	}

	#[test]
	fn plot_click_and_drag_display_submitted_frequency() {
		let ctx = egui::Context::default();
		let host = DelayedHost::default();
		let param = IkuperserParams::default().frequency;
		let mut state = ControlState::default();
		let mut now = 0.0;
		let mut frame = |x: f32, button: Option<bool>| {
			now += 1.0 / 60.0;
			let pos = Pos2::new(x, 80.0);
			let mut events = vec![egui::Event::PointerMoved(pos)];
			if let Some(pressed) = button {
				events.push(egui::Event::PointerButton {
					pos,
					button: PointerButton::Primary,
					pressed,
					modifiers: egui::Modifiers::NONE,
				});
			}
			let _ = ctx.run(
				egui::RawInput {
					screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 420.0))),
					time: Some(now),
					events,
					..Default::default()
				},
				|ctx| {
					egui::CentralPanel::default().show(ctx, |ui| {
						let (rect, response) = ui.allocate_exact_size(Vec2::new(592.0, 220.0), Sense::click_and_drag());
						plot_input(ui, &param, &ParamSetter::new(&host), &mut state, &response);
						if response.clicked() || response.dragged() || response.drag_stopped() {
							let expected = x_to_frequency(rect.shrink2(Vec2::new(8.0, 18.0)), x);
							assert!((state.plain_value(&param, now) / expected - 1.0).abs() < 1.0e-5);
						}
					});
				},
			);
		};
		frame(100.0, None);
		frame(100.0, Some(true));
		frame(100.0, Some(false));
		frame(100.0, Some(true));
		frame(350.0, None);
		frame(550.0, None);
		frame(600.0, Some(false));
		assert!(state.drag.is_none());
		assert_eq!(state.plain_value(&param, now), MAX_FREQUENCY);
		assert_eq!(param.value(), 1000.0);
		let events = host.events.lock().unwrap();
		assert_eq!(events.iter().filter(|e| matches!(e, HostEvent::Begin)).count(), 2);
		assert_eq!(events.iter().filter(|e| matches!(e, HostEvent::End)).count(), 2);
	}

	#[test]
	fn fine_drag_preserves_sub_step_amount_movement() {
		let mut gui = KnobHarness::new(IkuperserParams::default().amount);
		let start = gui.param.unmodulated_normalized_value();
		gui.frame(80.0, Some((PointerButton::Primary, true)), false);
		gui.frame(72.0, None, false);
		for i in 1..=160 {
			gui.frame(72.0 - i as f32 * 0.125, None, true);
		}
		let target = start + 8.0 / KNOB_DRAG_RANGE + 20.0 / (KNOB_DRAG_RANGE * FINE_DRAG_DIVISOR);
		let expected = gui.param.preview_plain(target);
		assert_eq!(gui.state.plain_value(&gui.param, gui.now), expected);
		let values = gui.sent_values();
		assert!(values.len() < 160);
		assert!(values.last().unwrap() > &values[0]);
		for normalized in values {
			let plain = gui.param.preview_plain(normalized);
			assert_eq!(plain, plain.round());
			assert!((gui.param.preview_normalized(plain) - normalized).abs() < 1.0e-6);
		}
	}

	#[test]
	fn shift_switching_does_not_jump() {
		let mut drag = DragState {
			normalized: 0.5,
			pointer_y: 100.0,
		};
		assert!((drag.move_to(90.0, false) - 0.55).abs() < 1.0e-6);
		assert!((drag.move_to(90.0, true) - 0.55).abs() < 1.0e-6);
		assert!((drag.move_to(80.0, true) - 0.555).abs() < 1.0e-6);
		assert!((drag.move_to(80.0, false) - 0.555).abs() < 1.0e-6);
		assert!((drag.move_to(70.0, false) - 0.605).abs() < 1.0e-6);
	}

	#[test]
	fn drag_distance_is_independent_of_frame_count() {
		let mut one_frame = DragState {
			normalized: 0.25,
			pointer_y: 100.0,
		};
		let mut many_frames = DragState {
			normalized: 0.25,
			pointer_y: 100.0,
		};
		one_frame.move_to(0.0, false);
		for y in (0..100).rev() {
			many_frames.move_to(y as f32, false);
		}
		assert!((one_frame.normalized - many_frames.normalized).abs() < 1.0e-6);
		let value = many_frames.normalized;
		assert_eq!(many_frames.move_to(0.0, false), value);
	}

	#[test]
	fn reversing_at_limit_responds_immediately() {
		let mut drag = DragState {
			normalized: 0.99,
			pointer_y: 100.0,
		};
		assert_eq!(drag.move_to(0.0, false), 1.0);
		assert!((drag.move_to(1.0, false) - 0.995).abs() < 1.0e-6);
		assert_eq!(drag.move_to(1000.0, false), 0.0);
		assert!((drag.move_to(999.0, false) - 0.005).abs() < 1.0e-6);
	}

	#[test]
	fn pending_value_expires_if_host_rejects_change() {
		let param = continuous_param(0.5);
		let mut state = ControlState {
			drag: None,
			pending: Some(PendingValue {
				normalized: 0.65,
				since: 1.0,
			}),
		};
		assert_eq!(state.normalized(&param, 1.1), 0.65);
		assert_eq!(state.normalized(&param, 1.0 + HOST_VALUE_TIMEOUT), 0.5);
		assert!(state.pending.is_none());
	}

	#[test]
	fn acknowledged_value_resumes_host_updates() {
		let mut state = ControlState {
			drag: None,
			pending: Some(PendingValue {
				normalized: 0.65,
				since: 1.0,
			}),
		};
		assert_eq!(state.normalized(&continuous_param(0.65), 1.1), 0.65);
		assert!(state.pending.is_none());
		assert_eq!(state.normalized(&continuous_param(0.7), 1.2), 0.7);
	}

	#[test]
	fn secondary_button_does_not_edit_parameter() {
		let mut gui = KnobHarness::new(continuous_param(0.5));
		gui.frame(80.0, Some((PointerButton::Secondary, true)), false);
		gui.frame(50.0, None, false);
		gui.frame(50.0, Some((PointerButton::Secondary, false)), false);
		assert!(gui.host.events.lock().unwrap().is_empty());
		assert!(gui.state.drag.is_none());
	}

	fn plot_rect() -> Rect {
		Rect::from_min_size(Pos2::ZERO, Vec2::new(592.0, 184.0))
	}

	#[test]
	fn plot_range_is_continuous_across_rounding_boundaries() {
		for boundary in [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 100.0] {
			let below = delay_plot_range(&[(0.0, boundary * 0.999)]);
			let above = delay_plot_range(&[(0.0, boundary * 1.001)]);
			assert!((above / below - 1.0).abs() < 0.003);
		}
	}

	#[test]
	fn changing_amount_changes_curve_height_without_rescaling_axis() {
		let (reference, reference_range) = delay_plot_data(plot_rect(), 1000.0, 4.0, 1.0, SAMPLE_RATE);
		let reference_peak = reference.iter().map(|&(_, delay)| delay).fold(0.0, f32::max);
		let mut previous_height = reference_peak / reference_range;
		for amount in [2.0, 16.0, 50.0, MAX_STAGES as f32] {
			let (delays, range) = delay_plot_data(plot_rect(), 1000.0, 4.0, amount, SAMPLE_RATE);
			assert_eq!(range, reference_range);
			let peak = delays.iter().map(|&(_, delay)| delay).fold(0.0, f32::max);
			let height = peak / range;
			assert!(height > previous_height && height < 1.0);
			assert!((height / (reference_peak / reference_range) - amount).abs() < 1.0e-4);
			previous_height = height;
		}
	}

	#[test]
	fn narrow_peak_is_sampled_between_grid_points() {
		let plot = plot_rect();
		for offset in [0.0, 0.25, 0.5, 0.75] {
			let x = plot.left() + plot.width() * (700.0 + offset) / (PLOT_POINTS - 1) as f32;
			let center = x_to_frequency(plot, x);
			let delays = delay_curve(plot, center, 20.0, 16.0, SAMPLE_RATE);
			let peak = group_delay_ms(center, center, 20.0, 16.0, SAMPLE_RATE);
			assert!(delays.windows(2).all(|pair| pair[0].0 <= pair[1].0));
			assert!(delays.iter().all(|&(x, delay)| x.is_finite() && delay.is_finite()));
			assert!(
				delays
					.iter()
					.any(|&(sample_x, delay)| { (sample_x - x).abs() < 1.0e-3 && (delay / peak - 1.0).abs() < 1.0e-4 })
			);
		}
	}

	#[test]
	fn zero_amount_has_finite_plot_scale() {
		let (delays, range) = delay_plot_data(plot_rect(), 1000.0, 1.0, 0.0, SAMPLE_RATE);
		let (_, nonzero_range) = delay_plot_data(plot_rect(), 1000.0, 1.0, 1.0, SAMPLE_RATE);
		assert!(range.is_finite() && range > 0.0);
		assert_eq!(range, nonzero_range);
		assert!(delays.iter().all(|&(_, delay)| delay / range == 0.0));
	}
}
