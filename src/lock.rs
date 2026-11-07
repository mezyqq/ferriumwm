//! ext-session-lock (swaylock, hyprlock, noctalia lock screen...).

use smithay::{
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::SERIAL_COUNTER,
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};
use tracing::info;

use crate::state::Fe;

#[derive(Default)]
pub struct LockState {
    pub locked: bool,
    pub surfaces: Vec<(Output, LockSurface)>,
}

impl LockState {
    pub fn surface_for(&self, output: &Output) -> Option<&LockSurface> {
        self.surfaces.iter().find(|(o, _)| o == output).map(|(_, s)| s)
    }
}

impl SessionLockHandler for Fe {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        info!("экран заблокирован");
        self.lock.locked = true;
        self.lock.surfaces.clear();
        let kb = self.seat.get_keyboard().unwrap();
        kb.set_focus(self, None::<WlSurface>, SERIAL_COUNTER.next_serial());
        confirmation.lock();
        self.queue_redraw();
    }

    fn unlock(&mut self) {
        info!("экран разблокирован");
        self.lock.locked = false;
        self.lock.surfaces.clear();
        let f = self.focus;
        self.focus = None;
        self.focus_window(f, false);
        self.queue_redraw();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else { return };
        let size = output
            .current_mode()
            .map(|m| {
                let s = output.current_scale().fractional_scale();
                let t = output.current_transform();
                t.transform_size(m.size).to_f64().to_logical(s).to_i32_round::<i32>()
            })
            .unwrap_or_else(|| (1920, 1080).into());
        surface.with_pending_state(|s| s.size = Some((size.w as u32, size.h as u32).into()));
        surface.send_configure();
        let wl = surface.wl_surface().clone();
        self.lock.surfaces.retain(|(o, _)| *o != output);
        self.lock.surfaces.push((output.clone(), surface));
        let focused = self.outputs.get(self.focused_output).map(|o| o.output == output).unwrap_or(true);
        if focused || self.lock.surfaces.len() == 1 {
            let kb = self.seat.get_keyboard().unwrap();
            kb.set_focus(self, Some(wl), SERIAL_COUNTER.next_serial());
        }
        self.queue_redraw();
    }
}
