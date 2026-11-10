//! The bar: one wlr-layer-shell surface, the Wayland events it reacts to, and drawing.

use std::time::{Duration, Instant};

use calloop::{
    LoopHandle, LoopSignal,
    timer::{TimeoutAction, Timer},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            Connection, Dispatch, QueueHandle, delegate_noop,
            globals::GlobalList,
            protocol::{wl_output, wl_shm, wl_surface},
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use tiny_skia::{Color, ColorU8, PixmapMut};
use tracing::{error, info, warn};

use crate::text::Text;

/// Bar height and exclusive zone, in logical pixels.
pub const HEIGHT: u32 = 30;
/// Space left of the text, in logical pixels.
const PADDING_LEFT: f32 = 8.0;
/// Colours as sRGB `[red, green, blue]`.
const BACKGROUND: [u8; 3] = [0x16, 0x18, 0x1d];
const FOREGROUND: [u8; 3] = [0xe6, 0xe8, 0xee];
/// How long after the first frame to log VmRSS.
const RSS_REPORT_DELAY: Duration = Duration::from_secs(10);
/// Starting size of the shared-memory pool: one 1920×30 bar. sctk grows it if needed.
const INITIAL_POOL_BYTES: usize = 1920 * HEIGHT as usize * 4;
/// Sanity limits for numbers the compositor sends us. Anything outside them is logged and ignored.
const MAX_WIDTH: u32 = 16_384;
const MAX_SCALE_120: u32 = 8 * 120;

/// Everything the bar owns. calloop hands a `&mut Bar` to every event callback,
/// so all state lives here and no `Rc<RefCell<..>>` sharing is needed.
pub struct Bar {
    started: Instant,
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    output: wl_output::WlOutput,
    /// `Some` only when the compositor offers both wp_fractional_scale_v1 and
    /// wp_viewporter. `None` means we use integer `wl_surface.set_buffer_scale`.
    fractional: Option<(WpFractionalScaleV1, WpViewport)>,
    text: Text,
    loop_handle: LoopHandle<'static, Bar>,
    loop_signal: LoopSignal,
    /// Logical width from the last configure. 0 until the first configure arrives.
    width: u32,
    /// Current scale in 120ths (120 = 1.0), the unit wp_fractional_scale_v1 uses.
    scale_120: u32,
    /// The buffer we last committed. Replacing it drops the old one, which sctk
    /// destroys once the compositor releases it.
    buffer: Option<Buffer>,
    first_frame_done: bool,
    /// Kept open after the spike's one call, the way a real bar would keep it.
    dbus: Option<zbus::Connection>,
}

impl Bar {
    /// Binds the globals, creates the layer surface on the first output and sends
    /// the initial empty commit that asks the compositor for a configure.
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Bar>,
        text: Text,
        loop_handle: LoopHandle<'static, Bar>,
        loop_signal: LoopSignal,
        started: Instant,
    ) -> Bar {
        let compositor =
            CompositorState::bind(globals, qh).expect("compositor does not offer wl_compositor");
        let layer_shell =
            LayerShell::bind(globals, qh).expect("compositor does not offer zwlr_layer_shell_v1");
        let shm = Shm::bind(globals, qh).expect("compositor does not offer wl_shm");
        let outputs = OutputState::new(globals, qh);
        let output = outputs
            .outputs()
            .next()
            .expect("compositor announced no outputs");

        let surface = compositor.create_surface(qh);
        let fractional = bind_fractional_scale(globals, qh, &surface);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Top,
            Some("crabshell-spike"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        // Width 0 means "compositor decides", which with left+right anchors is the output width.
        layer.set_size(0, HEIGHT);
        layer.set_exclusive_zone(HEIGHT as i32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        let pool = SlotPool::new(INITIAL_POOL_BYTES, &shm)
            .expect("failed to create the wl_shm memory pool");

        Bar {
            started,
            registry: RegistryState::new(globals),
            outputs,
            shm,
            pool,
            layer,
            output,
            fractional,
            text,
            loop_handle,
            loop_signal,
            width: 0,
            scale_120: 120,
            buffer: None,
            first_frame_done: false,
            dbus: None,
        }
    }

    /// Asks the event loop to return, which ends `main`.
    pub fn exit(&self) {
        self.loop_signal.stop();
    }

    /// Stores the D-Bus connection so it stays open.
    pub fn keep_dbus(&mut self, connection: zbus::Connection) {
        self.dbus = Some(connection);
    }

    fn scale_method(&self) -> &'static str {
        if self.fractional.is_some() {
            "fractional (wp_fractional_scale_v1 + wp_viewporter)"
        } else {
            "integer (wl_surface.set_buffer_scale)"
        }
    }

    fn set_scale(&mut self, scale_120: u32, conn: &Connection) {
        if scale_120 == 0 || scale_120 > MAX_SCALE_120 {
            warn!(scale_120, "ignoring out-of-range scale from the compositor");
            return;
        }
        if scale_120 == self.scale_120 {
            return;
        }
        self.scale_120 = scale_120;
        info!(scale = scale_120 as f32 / 120.0, "scale changed");
        if self.width != 0 {
            self.draw(conn);
        }
    }

    /// Paints one frame into a fresh shm buffer and commits it.
    fn draw(&mut self, conn: &Connection) {
        let width = to_buffer_pixels(self.width, self.scale_120);
        let height = to_buffer_pixels(HEIGHT, self.scale_120);
        let stride = width * 4;

        let (buffer, canvas) = match self.pool.create_buffer(
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                error!(%err, width, height, "could not allocate a shm buffer");
                return;
            }
        };

        // `canvas` is a `&mut [u8]` pointing straight into the shared memory the
        // compositor reads. `PixmapMut` borrows it (that is its `'_` lifetime) and
        // draws in place: no copy. While that borrow of `self.pool` is alive we may
        // still use other fields (`self.text`), but not methods that take all of
        // `self`. It ends after the last use of `pixmap`, so the `self.scale_method()`
        // call further down compiles.
        let Some(mut pixmap) = PixmapMut::from_bytes(canvas, width, height) else {
            error!(width, height, "shm canvas is smaller than the pixmap needs");
            return;
        };
        let scale = self.scale_120 as f32 / 120.0;
        pixmap.fill(shm_color(BACKGROUND));
        self.text
            .draw(&mut pixmap, PADDING_LEFT, scale, shm_color_u8(FOREGROUND));

        let surface = self.layer.wl_surface();
        match &self.fractional {
            Some((_, viewport)) => viewport.set_destination(self.width as i32, HEIGHT as i32),
            None => surface.set_buffer_scale((self.scale_120 / 120) as i32),
        }
        if let Err(err) = buffer.attach_to(surface) {
            error!(%err, "could not attach the shm buffer");
            return;
        }
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();
        self.buffer = Some(buffer);
        if let Err(err) = conn.flush() {
            warn!(%err, "could not flush the Wayland connection");
        }

        if !self.first_frame_done {
            self.first_frame_done = true;
            info!(
                buffer_width = width,
                buffer_height = height,
                scale,
                method = self.scale_method(),
                "first frame committed {:.2} ms after main() started",
                self.started.elapsed().as_secs_f64() * 1000.0
            );
            self.schedule_rss_report();
        } else {
            info!(
                buffer_width = width,
                buffer_height = height,
                scale,
                "redrew"
            );
        }
    }

    fn schedule_rss_report(&self) {
        let timer = Timer::from_duration(RSS_REPORT_DELAY);
        let inserted = self.loop_handle.insert_source(timer, |_deadline, _, _bar| {
            match vm_rss_kib() {
                Some(kib) => info!(
                    vm_rss_kib = kib,
                    "VmRSS {:.1} MiB, {} s after the first frame",
                    kib as f64 / 1024.0,
                    RSS_REPORT_DELAY.as_secs()
                ),
                None => warn!("could not read VmRSS from /proc/self/status"),
            }
            TimeoutAction::Drop
        });
        if let Err(err) = inserted {
            warn!(%err, "could not schedule the RSS report");
        }
    }
}

/// Binds fractional scaling when the compositor offers both protocols it needs.
fn bind_fractional_scale(
    globals: &GlobalList,
    qh: &QueueHandle<Bar>,
    surface: &wl_surface::WlSurface,
) -> Option<(WpFractionalScaleV1, WpViewport)> {
    let manager = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(qh, 1..=1, ())
        .ok()?;
    let Ok(viewporter) = globals.bind::<WpViewporter, _, _>(qh, 1..=1, ()) else {
        manager.destroy();
        return None;
    };
    let fractional_scale = manager.get_fractional_scale(surface, qh, ());
    let viewport = viewporter.get_viewport(surface, qh, ());
    // Both protocols say destroying the factory leaves the objects it made alive.
    manager.destroy();
    viewporter.destroy();
    Some((fractional_scale, viewport))
}

/// Logical pixels to buffer pixels, rounding half up as wp_fractional_scale_v1 asks.
/// Inputs are already range-checked, so this can't overflow.
fn to_buffer_pixels(logical: u32, scale_120: u32) -> u32 {
    (logical * scale_120 + 60) / 120
}

/// wl_shm's Argb8888 stores each pixel as the bytes B, G, R, A (a little-endian
/// 32-bit ARGB word), while tiny-skia writes R, G, B, A. Swapping red and blue in
/// every colour we paint gives the right bytes with no conversion pass. Blending
/// treats channels independently, so the result is exact.
fn shm_color([red, green, blue]: [u8; 3]) -> Color {
    Color::from_rgba8(blue, green, red, 255)
}

fn shm_color_u8([red, green, blue]: [u8; 3]) -> ColorU8 {
    ColorU8::from_rgba(blue, green, red, 255)
}

/// Resident set size in KiB, read from the `VmRSS:` line of /proc/self/status.
fn vm_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

impl LayerShellHandler for Bar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        info!("compositor closed the layer surface, exiting");
        self.exit();
    }

    fn configure(
        &mut self,
        conn: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // sctk has already acked this configure before calling us.
        let (width, height) = configure.new_size;
        if width == 0 || width > MAX_WIDTH {
            warn!(width, "ignoring configure with an unusable width");
            return;
        }
        if !self.first_frame_done {
            let output = self.outputs.info(&self.output).and_then(|info| info.name);
            info!(
                width,
                height,
                output = output.as_deref().unwrap_or("unknown"),
                "first configure"
            );
        } else if width == self.width {
            // Nothing in the committed frame depends on what changed, so skip
            // the repaint. The commit applies the ack sctk already sent.
            self.layer.commit();
            info!(
                width,
                "configure with unchanged size, committed without redrawing"
            );
            return;
        }
        self.width = width;
        self.draw(conn);
    }
}

impl CompositorHandler for Bar {
    /// Integer scale from sctk (wl_surface.preferred_buffer_scale, or the outputs
    /// the surface is on). Used only when fractional scaling isn't available.
    fn scale_factor_changed(
        &mut self,
        conn: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if self.fractional.is_some() {
            return;
        }
        match u32::try_from(new_factor) {
            Ok(factor) if factor <= MAX_SCALE_120 / 120 => self.set_scale(factor * 120, conn),
            _ => warn!(new_factor, "ignoring out-of-range integer scale"),
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    /// Never called: the bar never requests frame callbacks, because nothing animates.
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Bar {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        // If it was our output, the compositor also sends `closed`, which exits.
    }
}

impl ShmHandler for Bar {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Bar {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState];
}

impl Dispatch<WpFractionalScaleV1, ()> for Bar {
    fn event(
        bar: &mut Bar,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Bar>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            bar.set_scale(scale, conn);
        }
    }
}

// These three interfaces have no events, so their dispatch is a no-op.
delegate_noop!(Bar: WpFractionalScaleManagerV1);
delegate_noop!(Bar: WpViewporter);
delegate_noop!(Bar: WpViewport);

// sctk 0.21: one macro routes every sctk-managed object's events to the handler traits above.
delegate_dispatch2!(Bar);
delegate_registry!(Bar);
