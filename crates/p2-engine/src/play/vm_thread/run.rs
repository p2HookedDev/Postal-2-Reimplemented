use super::*;

impl Simulation {
    fn set_vm_profile(&mut self, profile: VmProfile) {
        if profile == VmProfile::TickByClass {
            self.vm.diagnostics.tick_profile.get_or_insert_default();
        }
        if profile == VmProfile::Work {
            if self.work_since.is_none() {
                self.vm.diagnostics.count_work = true;
                self.vm.profile_functions(p2_vm::FunctionProfiling::On);
                self.work_since = Some((Instant::now(), 0));
            }
        } else if self.work_since.take().is_some() {
            self.vm.diagnostics.count_work = false;
            self.vm.profile_functions(p2_vm::FunctionProfiling::Off);
            self.work_second = WorkSecond::default();
        }
    }

    pub(in crate::play) fn run(&mut self, mut job: Job) -> VmFrame {
        let mut frame = std::mem::take(&mut job.frame).recycle(job.step);
        let playing = job.playing && self.player_pawn.is_some();
        self.vm.options.player_physics = playing;
        self.set_vm_profile(job.vm_profile);
        let started = Instant::now();
        for &MovieStatus { movie, width, height, playing } in &job.movie_status {
            self.vm.set_movie_status(movie, width, height, playing);
        }
        for &actor in &job.finished {
            if let Err(error) = self.vm.destroy(actor) {
                bevy::log::warn!("{actor:?}: {error}");
            }
        }
        if playing && let Err(error) = job.key_events.iter().try_for_each(|&(key, pressed)| self.vm.key_event(key, pressed)).and_then(|()| self.vm.player_input(&job.held, job.mouse)) {
            bevy::log::warn!("input: {error}");
        }
        let errors = self.vm.tick_level(job.step);
        frame.times.push(("vm tick", started.elapsed()));
        for (actor, error) in errors.iter().take(3) {
            bevy::log::warn!("{actor}: {error}");
        }
        // Each distinct line once: a script error repeated every tick would flood the log.
        for line in self.vm.diagnostics.log.drain(..) {
            if !self.logged.contains(&line) {
                bevy::log::info!("vm: {line}");
                self.logged.insert(line);
            }
        }
        let started = Instant::now();
        // Played, the player's controller decides the view (`PlayerCalcView`: first person, a Matinee camera, ...),
        // before the actors are read: what is drawn depends on it.
        if job.playing {
            match self.vm.player_view() {
                Ok(Some(view)) => {
                    self.view = Some(view);
                    frame.view = Some(view);
                }
                Ok(None) => {}
                Err(error) => bevy::log::warn!("view: {error}"),
            }
        }
        let commands: Vec<_> = self.vm.take_commands(|command| matches!(command, HostCommand::Sound(_) | HostCommand::Music(_))).collect();
        let at = |actor: Option<ObjectId>| actor.map(|actor| self.vm.store.location(actor));
        for command in commands {
            match command {
                HostCommand::Sound(played) => {
                    let position = at(played.actor);
                    frame.sounds.push((played, position));
                }
                HostCommand::Music(command) => {
                    let (actor, song) = match &command {
                        MusicCommand::Play { actor, song, .. } => (*actor, read_song(&self.music_dir, song)),
                        _ => (None, None),
                    };
                    frame.music.push((command, at(actor), song));
                }
                _ => {}
            }
        }
        // Actors a camera saw last frame are marked rendered, or POSTAL 2's pawns go into stasis.
        for &actor in &job.visible {
            if let Err(error) = self.vm.mark_rendered(actor) {
                bevy::log::warn!("{actor:?}: {error}");
            }
        }
        for &(actor, BoneFrame { location, axes }) in &job.placed {
            self.vm.place_attachment(actor, location, axes.map(Into::into));
        }
        frame.times.push(("vm view", started.elapsed()));
        let started = Instant::now();
        let followed = &job.followed;
        let mut recent = std::mem::take(&mut self.recent_read);
        recent.clear();
        recent.extend(self.recent.iter().flatten().copied().filter(|actor| !followed.contains(actor)));
        for &actor in followed.iter().chain(&recent) {
            self.pass_actor(actor, &mut frame.actor_changes, &mut frame.spawned);
        }
        frame.times.push(("vm read actors", started.elapsed()));
        let started = Instant::now();
        for &actor in followed {
            if job.visible.contains(&actor) || !self.posed.contains(&actor) {
                self.read_pose(actor, &mut frame);
            }
        }
        for &actor in &recent {
            self.read_pose(actor, &mut frame);
        }
        self.recent_read = recent;
        frame.times.push(("vm read poses", started.elapsed()));
        // The client's input (menus, console), then the HUD; a map change goes first, as retail loads the
        // map before it draws again.
        self.vm.set_video_modes(crate::display::video_modes());
        if let Some([x, y]) = job.cursor {
            self.vm.set_mouse_position(x, y);
        }
        frame.shows_mouse = self.vm.shows_mouse();
        if !job.typed.is_empty()
            && let Err(error) = self.vm.typed(&job.typed)
        {
            bevy::log::warn!("typed: {error}");
        }
        frame.travel = self
            .vm
            .take_commands(|command| matches!(command, HostCommand::Travel(_)))
            .filter_map(|command| if let HostCommand::Travel(url) = command { Some(url) } else { None })
            .last();
        if frame.travel.is_none() {
            let started = Instant::now();
            frame.hud = self.hud(job.canvas).into();
            frame.times.push(("hud scripts", started.elapsed()));
        }
        let started = Instant::now();
        self.read_after_hud(&mut frame, &job.watched, job.forces);
        frame.times.push(("vm read after hud", started.elapsed()));
        frame.profile = self.vm.diagnostics.tick_profile.take().unwrap_or_default();
        let vm_time = frame.times.iter().map(|(_, time)| *time).sum();
        self.log_work(vm_time);
        frame.job_buffers = JobBuffers { held: job.held, followed: job.followed, visible: job.visible, placed: job.placed, watched: job.watched };
        frame
    }

    fn read_pose(&mut self, actor: ObjectId, frame: &mut VmFrame) {
        let pose = self.actor_pose(actor);
        frame.poses.insert(actor, pose);
        if let Some(bones) = self.vm.ragdoll_pose(actor) {
            frame.ragdolls.insert(actor, bones);
        }
        self.posed.insert(actor);
    }

    pub(in crate::play) fn log_work(&mut self, vm_time: Duration) {
        let Some((since, frames)) = &mut self.work_since else { return };
        *frames += 1;
        let (since, frames) = (*since, *frames);
        let counts = self.vm.take_work_counts();
        let functions = self.vm.take_function_profile();
        let second = &mut self.work_second;
        if second.counts.is_empty() {
            second.counts = counts.iter().map(|&(name, _)| (name, 0)).collect();
        }
        for (sum, (_, n)) in second.counts.iter_mut().zip(&counts) {
            sum.1 += n;
        }
        for (name, time) in &functions {
            let sum = second.functions.entry(name.clone()).or_default();
            (sum.calls, sum.total, sum.own, sum.native) = (sum.calls + time.calls, sum.total + time.total, sum.own + time.own, time.native);
        }
        if second.slowest.as_ref().is_none_or(|(slowest, ..)| vm_time > *slowest) {
            second.slowest = Some((vm_time, counts, functions));
        }
        if since.elapsed() < Duration::from_secs(1) {
            return;
        }
        let per = f64::from(frames);
        self.work_since = Some((Instant::now(), 0));
        let second = std::mem::take(&mut self.work_second);
        let work = |counts: &[(&str, u64)], per: f64| -> String {
            counts.iter().filter(|(_, n)| *n > 0).map(|(name, n)| format!("{name} {:.0}", *n as f64 / per)).collect::<Vec<_>>().join(", ")
        };
        let top = |functions: &mut Vec<(String, p2_vm::FunctionTime)>, per: f64| -> String {
            functions.sort_by_key(|(_, time)| std::cmp::Reverse(time.own));
            let ms = |time: Duration| time.as_secs_f64() * 1000.0 / per;
            functions
                .iter()
                .take(20)
                .map(|(name, time)| format!("{name} {:.3} ({:.0} calls, {:.3} total)", ms(time.own), time.calls as f64 / per, ms(time.total)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        bevy::log::info!("vm work per frame: {}", work(&second.counts, per));
        let (placed, always, stale) = self.vm.collision_hash_census();
        bevy::log::info!("vm collision hash: {placed} actors, {always} always candidates, {stale} stale");
        let names: Vec<String> = self.vm.take_property_names().iter().take(15).map(|(name, n)| format!("{name} {:.0}", *n as f64 / per)).collect();
        bevy::log::info!("vm properties by name per frame: {}", names.join(", "));
        let mut functions: Vec<(String, p2_vm::FunctionTime)> = second.functions.into_iter().collect();
        bevy::log::info!("vm functions by own time (ms per frame): {}", top(&mut functions, per));
        if let Some((time, counts, mut functions)) = second.slowest {
            bevy::log::info!(
                "slowest vm frame {:.2} ms: work: {}; functions by own time (ms): {}",
                time.as_secs_f64() * 1000.0,
                work(&counts, 1.0),
                top(&mut functions, 1.0)
            );
        }
    }
}
