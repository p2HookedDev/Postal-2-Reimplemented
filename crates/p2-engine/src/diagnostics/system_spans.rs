use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use bevy::ecs::prelude::Resource;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing::Subscriber;
use bevy::log::tracing_subscriber::Layer;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::registry::LookupSpan;
use rustc_hash::FxHashMap;

#[derive(Resource, Clone, Default)]
pub struct SystemSpans(Arc<Mutex<FxHashMap<Arc<str>, [f64; 3]>>>);

#[derive(Default)]
pub(super) struct SpanSecond {
    pub(super) slowest: Vec<(Arc<str>, f64, f64)>,
    pub(super) longest: Vec<(Arc<str>, f64, f64)>,
    pub(super) wgpu: Vec<(Arc<str>, f64, f64)>,
}

pub(super) const WGPU: &str = "wgpu ";

impl SystemSpans {
    pub(super) fn take_second(&self, count: usize, longest: usize, frames: u32) -> SpanSecond {
        let mut spent = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let frames = f64::from(frames.max(1));
        let (wgpu, systems): (Vec<_>, Vec<_>) = spent.drain().partition(|(name, _)| name.starts_with(WGPU));
        let ranked = |list: &[(Arc<str>, [f64; 3])], which: usize, count: usize, over: f64| {
            let mut ranked: Vec<_> = list.iter().map(|(name, [total, long, runs])| (name.clone(), [*total, *long][which] / over, runs / frames)).collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
            ranked.truncate(count);
            ranked
        };
        SpanSecond { slowest: ranked(&systems, 0, count, frames), longest: ranked(&systems, 1, longest, 1.0), wgpu: ranked(&wgpu, 0, usize::MAX, frames) }
    }
}

struct SpanClock {
    name: Arc<str>,
    entered: Option<Instant>,
}

struct FieldText {
    wanted: &'static str,
    text: String,
}

impl Visit for FieldText {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == self.wanted {
            value.clone_into(&mut self.text);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == self.wanted {
            self.text = format!("{value:?}");
        }
    }
}

fn label(attrs: &Attributes<'_>) -> Option<String> {
    let field = |wanted| {
        let mut field = FieldText { wanted, text: String::new() };
        attrs.record(&mut field);
        field.text
    };
    let meta = attrs.metadata();
    Some(match meta.name() {
        "system" => field("name"),
        "system_commands" => format!("commands of {}", field("name")),
        "finish_command_buffer" => format!("encode {}", field("system")),
        "camera_schedule" => field("camera"),
        name if meta.target().starts_with("wgpu") => format!("{WGPU}{name}"),
        _ => return None,
    })
}

pub(super) struct SpanTiming(pub(super) SystemSpans);

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for SpanTiming {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(name) = label(attrs) else { return };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanClock { name: name.into(), entered: None });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(clock) = span.extensions_mut().get_mut::<SpanClock>()
        {
            clock.entered = Some(Instant::now());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        let Some(clock) = extensions.get_mut::<SpanClock>() else { return };
        let Some(entered) = clock.entered.take() else { return };
        let ms = entered.elapsed().as_secs_f64() * 1000.0;
        let mut spent = self.0.0.lock().unwrap_or_else(PoisonError::into_inner);
        let [total, longest, runs] = spent.entry(clock.name.clone()).or_default();
        *total += ms;
        *longest = longest.max(ms);
        *runs += 1.0;
    }
}
