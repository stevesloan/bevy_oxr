//! `xrEndFrame` on its own thread.
//!
//! A runtime is free to throttle the application in `xrEndFrame` rather than
//! in `xrWaitFrame`: SteamVR on Linux returns from the wait at once and holds
//! the end call until just after vsync, four to six milliseconds at 90 Hz. Run
//! as a system, that block holds the render world, so the next frame's extract
//! cannot start until it is over, and extract and render together have to fit
//! between vsync and the compositor's deadline.
//!
//! With [`OxrFrameSubmitter`] the render world hands the call over and is free
//! at once. `xrBeginFrame` for the next frame still waits for it to return, as
//! OpenXR requires, and view poses are located after that, so they are as
//! fresh as before. Where a runtime doesn't block in `xrEndFrame`, the wait
//! costs nothing.
//!
//! The frame stream travels with the call and comes back when it returns, so
//! it is in exactly one place at a time: the render world, or here.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use bevy_ecs::{resource::Resource, world::World};
use bevy_log::error;

use crate::{
    layer_builder::DetachedLayer,
    resources::{OxrFrameStream, OxrQueueLock},
};

/// One `xrEndFrame`, with everything it needs and nothing borrowed.
pub struct EndFrame {
    pub stream: OxrFrameStream,
    pub display_time: openxr::Time,
    pub blend_mode: openxr::EnvironmentBlendMode,
    pub layers: Vec<Box<dyn DetachedLayer>>,
    /// `xrEndFrame` may use the application's Vulkan queue, so it runs under
    /// the same lock here as it would on the render thread.
    pub queue_lock: Option<OxrQueueLock>,
}

/// Runs `xrEndFrame` off the render thread. See the [module docs](self).
#[derive(Resource, Clone)]
pub struct OxrFrameSubmitter(Arc<Shared>);

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    job: Option<EndFrame>,
    /// The stream of the last call to finish, until the render world takes it
    /// back.
    returned: Option<OxrFrameStream>,
    /// A call is queued or running.
    in_flight: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic on either side leaves the state usable: every field is
        // written whole.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for OxrFrameSubmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl OxrFrameSubmitter {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("xr end frame".into())
            .spawn(move || run(&worker))
            .expect("Failed to spawn the xrEndFrame thread");
        Self(shared)
    }

    /// Start `frame`'s `xrEndFrame` and return without waiting for it.
    ///
    /// The caller must have taken back the previous frame's stream with
    /// [`wait_idle`](Self::wait_idle) first, which holding `frame.stream`
    /// already implies.
    pub fn submit(&self, frame: EndFrame) {
        let mut state = self.0.lock();
        state.job = Some(frame);
        state.in_flight = true;
        self.0.changed.notify_all();
    }

    /// Block until no `xrEndFrame` is queued or running, and take back the
    /// frame stream if one has come home.
    pub fn wait_idle(&self) -> Option<OxrFrameStream> {
        let mut state = self.0.lock();
        while state.in_flight {
            state = self
                .0
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.returned.take()
    }
}

fn run(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        let Some(mut frame) = state.job.take() else {
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        drop(state);

        #[cfg(target_os = "android")]
        jni::JavaVM::singleton()
            .unwrap()
            .attach_current_thread(|_env| -> jni::errors::Result<()> { Ok(()) })
            .unwrap();
        {
            let _queue = frame.queue_lock.as_ref().map(|lock| lock.lock());
            if let Err(e) =
                frame
                    .stream
                    .end_detached(frame.display_time, frame.blend_mode, &frame.layers)
            {
                error!("Failed to end frame stream: {e}");
            }
        }

        state = shared.lock();
        state.returned = Some(frame.stream);
        state.in_flight = false;
        shared.changed.notify_all();
    }
}

/// Wait out a frame end in flight and put its stream back in `world`.
///
/// Runs ahead of `xrBeginFrame`, and before the session's resources are
/// dropped.
pub fn reclaim_frame_stream(world: &mut World) {
    let Some(submitter) = world.get_resource::<OxrFrameSubmitter>().cloned() else {
        return;
    };
    if let Some(stream) = submitter.wait_idle() {
        world.insert_resource(stream);
    }
}

/// Wait out a frame end in flight without taking the stream: for the main
/// world, ahead of `xrEndSession`.
pub fn wait_for_frame_end(world: &mut World) {
    let Some(submitter) = world.get_resource::<OxrFrameSubmitter>().cloned() else {
        return;
    };
    let mut state = submitter.0.lock();
    while state.in_flight {
        state = submitter
            .0
            .changed
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner);
    }
}
