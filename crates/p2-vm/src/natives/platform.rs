use crate::native_id::Native;
use crate::{Error, HostCommand, LoaderRef, ObjectId, Prop, Value, Vm};
use super::{bool_arg, int_arg, milliseconds_since_2000, str_arg};

/// Indexed by `ESocialNetwork`.
const SOCIAL_URLS: [&str; 3] = ["https://www.facebook.com/rwsstudiosllc", "https://twitter.com/RWSstudios", "http://www.runningwithscissors.com/"];

pub(super) fn platform_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::ConsoleCommand => {
            let (answer, handled) = console_command(vm, str_arg(args, 0))?;
            if this.is_some_and(|this| vm.store.is_a(this, "Interaction")) { Value::Bool(handled) } else { Value::Str(answer) }
        }
        Native::IsDemoBuild | Native::PlatformIsSteamDeck | Native::PlatformIsMacOS | Native::PlatformIsUnix | Native::IsSteamBuild => Value::Bool(false),
        Native::SteamOwnsDLC | Native::SteamOwnsGame => Value::Bool(u32::try_from(int_arg(args, 0)).is_ok_and(|app| vm.options.steam_apps.contains(&app))),
        // Retail opens the store page in the Steam overlay; ours asks Steam to open it.
        Native::SteamViewStorePage => {
            vm.host.commands.push(HostCommand::OpenUrl(format!("steam://store/{}", int_arg(args, 0))));
            return Ok(None);
        }
        Native::LaunchSocialNetwork => {
            if let Some(url) = usize::try_from(int_arg(args, 0)).ok().and_then(|n| SOCIAL_URLS.get(n)) {
                vm.host.commands.push(HostCommand::OpenUrl((*url).to_owned()));
            }
            Value::Str(String::new())
        }
        Native::PlatformIsWindows => Value::Bool(true),
        Native::SteamGetWorkshopStatus | Native::GetBuildDate => Value::Str(String::new()),
        // The install's country, `[Shell.ShellRootWindow] InstalledCountry` (unverified).
        Native::GetCountryCode => Value::Str(vm.ini_name("Shell.ShellRootWindow.InstalledCountry").unwrap_or_else(|| "US".to_owned())),
        Native::SaveConfig | Native::StaticSaveConfig => {
            if let Some(this) = this {
                vm.save_config(this, native == Native::StaticSaveConfig);
            }
            return Ok(None);
        }
        Native::ResetConfig => return Ok(None),
        Native::FileSize => {
            let path = str_arg(args, 0).replace('\\', "/");
            let stem = std::path::Path::new(&path).file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
            let size = vm.loader.find(&stem).and_then(|file| std::fs::metadata(file).ok()).map_or(-1, |m| m.len() as i32);
            Value::Int(size)
        }
        // The VM has no `Entry` map, so the played level stands in (unverified).
        Native::GetEntryLevel => Value::Object(vm.level()),
        Native::GetLocalURL => Value::Str(vm.map_name.clone()),
        // Ignores its arguments, as retail. System time: days since 2000-01-01 (X), seconds into the day (Y)
        // and milliseconds (Z).
        Native::GetMillisecondsFrom => Value::Vector(milliseconds_since_2000(vm, this).into()),
        Native::ClientTravel => {
            vm.diagnostics.log.push(format!("ClientTravel {}", str_arg(args, 0)));
            let url = crate::TravelUrl::parse(str_arg(args, 0)).resolve(&vm.url_options, crate::ETravelType::from_byte(int_arg(args, 1) as u8)).text();
            vm.host.commands.push(HostCommand::Travel(url));
            vm.travel_items = bool_arg(args, 2);
            return Ok(None);
        }
        Native::Travel => {
            vm.diagnostics.log.push(format!("Travel {}", str_arg(args, 0)));
            let url = crate::TravelUrl::parse(str_arg(args, 0)).resolve(&vm.url_options, crate::ETravelType::Absolute).text();
            vm.host.commands.push(HostCommand::Travel(url));
            vm.travel_items = false;
            return Ok(None);
        }
        Native::Log | Native::Warn => {
            vm.diagnostics.log.push(str_arg(args, 0).to_owned());
            return Ok(None);
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

fn console_command<L: LoaderRef>(vm: &mut Vm<L>, command: &str) -> Result<(String, bool), Error> {
    if let Some(answer) = vm.input_command(command) {
        return Ok((answer, true));
    }
    let answer = match command.to_ascii_lowercase().as_str() {
        "getping" | "getcurrentdisplaynumber" | "getrenderdeviceogl" => "0".to_owned(),
        "getcurrentres" => {
            let size = |property, default| match vm.media.canvas.object.and_then(|canvas| vm.store.get(canvas, property)) {
                Some(Value::Int(size)) if size > 0 => size,
                _ => default,
            };
            format!("{}x{}", size(Prop::SizeX, 1024), size(Prop::SizeY, 768))
        }
        "getfullscreen" => (if vm.is_fullscreen() { "True" } else { "False" }).to_owned(),
        "getcurrentcolordepth" => "32".to_owned(),
        "getdisplaycount" => "1".to_owned(),
        "flush" => String::new(),
        "togglefullscreen" => {
            let fullscreen = !vm.is_fullscreen();
            vm.request_window(None, fullscreen);
            String::new()
        }
        "setfullscreenwindowed" => {
            let fullscreen = vm.is_fullscreen();
            vm.request_window(None, fullscreen);
            String::new()
        }
        lower if lower.starts_with("setres ") => {
            vm.set_res(&command["setres ".len()..]);
            String::new()
        }
        lower if lower.starts_with("setrenderdeviceogl ") => String::new(),
        lower if ["brightness ", "contrast ", "gamma "].iter().any(|p| lower.starts_with(p)) => {
            let (name, value) = command.split_once(' ').unwrap_or_default();
            vm.set_command(&format!("{} {name} {value}", crate::VIEWPORT_MANAGER));
            String::new()
        }
        lower if lower == "cinematics" || lower.starts_with("cinematics ") => {
            let arg = lower["cinematics".len()..].trim();
            let on = &mut vm.host.client.cinematics;
            *on = if arg.is_empty() { !*on } else { arg.parse::<f64>().map_or(0, |v| v as i32) != 0 };
            String::new()
        }
        lower if lower == "cinematicsratio" || lower.starts_with("cinematicsratio ") => {
            vm.host.client.cinematics_ratio = Some(lower["cinematicsratio".len()..].trim().parse().unwrap_or(0.0));
            String::new()
        }
        lower if lower.starts_with("setmouse ") => {
            let mut numbers = lower["setmouse ".len()..].split_whitespace().map(|n| n.parse().unwrap_or(0));
            vm.host.commands.push(HostCommand::SetMouse([numbers.next().unwrap_or(0), numbers.next().unwrap_or(0)]));
            String::new()
        }
        lower if lower.starts_with("getsupportedresolution") => vm.supported_resolution(lower),
        lower if lower.starts_with("getfiletimestr ") => vm.file_time_text(command["getfiletimestr ".len()..].trim()),
        "menucancel" => String::new(),
        lower if lower.starts_with("toggleime") => String::new(),
        "resetfirstrun" => {
            vm.save_setting("FirstRun", "FirstRun", "0");
            String::new()
        }
        lower if lower.starts_with("netspeed ") => {
            let speed: i32 = lower["netspeed ".len()..].trim().parse().unwrap_or(0);
            vm.set_command(&format!("Engine.Player ConfiguredInternetSpeed {speed}"));
            String::new()
        }
        "disconnect" => {
            vm.host.commands.push(HostCommand::Travel("Entry".to_owned()));
            String::new()
        }
        "showmouse" | "hidemouse" => {
            if let Some(viewport) = vm.viewport() {
                vm.store.set(viewport, Prop::bShowWindowsMouse, Value::Bool(command.eq_ignore_ascii_case("showmouse")));
            }
            String::new()
        }
        "exit" | "quit" => {
            vm.host.commands.push(HostCommand::Quit);
            String::new()
        }
        lower if lower.starts_with("open ") || lower.starts_with("start ") => {
            vm.host.commands.push(HostCommand::Travel(command.split_once(' ').map_or("", |(_, url)| url).trim().to_owned()));
            String::new()
        }
        lower if lower.starts_with("savegame ") => {
            let saved = match lower["savegame ".len()..].trim().parse() {
                Ok(slot) => vm.save_game(slot)?,
                Err(_) => false,
            };
            (if saved { "1" } else { "0" }).to_owned()
        }
        lower if lower.starts_with("checksavegame ") => {
            let found = lower["checksavegame ".len()..].trim().parse().is_ok_and(|slot| vm.save_exists(slot));
            (if found { "1" } else { "0" }).to_owned()
        }
        "getgmtime" => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs()).to_string(),
        lower if lower.starts_with("iskeybind ") => (if vm.is_key_bound(&command["iskeybind ".len()..]) { "1" } else { "0" }).to_owned(),
        lower if lower.starts_with("set ") => {
            vm.set_command(&command["set ".len()..]);
            String::new()
        }
        lower if lower.starts_with("get ") => vm.get_command(&command["get ".len()..])?,
        _ => {
            if !vm.player_console_exec(command)? {
                vm.diagnostics.log.push(format!("ConsoleCommand: {command}"));
                return Ok((String::new(), false));
            }
            String::new()
        }
    };
    Ok((answer, true))
}

#[cfg(test)]
mod tests {
    use p2_package::Loader;

    use crate::{HostCommand, Value, Vm};

    #[test]
    fn console_command_says_whether_it_ran() {
        let loader = Loader::new(&std::env::temp_dir(), &[]).unwrap();
        let mut vm = Vm::new(&loader);
        assert_eq!(super::console_command(&mut vm, "flush").unwrap(), (String::new(), true));
        assert_eq!(super::console_command(&mut vm, "nosuchcommand").unwrap(), (String::new(), false));
    }

    #[test]
    fn steam_apps_answer_ownership_and_links_go_to_the_host() {
        let loader = Loader::new(&std::env::temp_dir(), &[]).unwrap();
        let mut vm = Vm::new(&loader);
        vm.options.steam_apps = vec![707030];
        let mut owns = |native: &str, app: i32| vm.call_native(native, None, &mut [Value::Int(app)]).unwrap();
        assert_eq!(owns("SteamOwnsGame", 707030), Some(Value::Bool(true)));
        assert_eq!(owns("SteamOwnsDLC", 360960), Some(Value::Bool(false)));
        assert_eq!(owns("SteamOwnsDLC", -1), Some(Value::Bool(false)));
        assert_eq!(vm.call_native("LaunchSocialNetwork", None, &mut [Value::Byte(1)]).unwrap(), Some(Value::Str(String::new())));
        vm.call_native("SteamViewStorePage", None, &mut [Value::Int(707030)]).unwrap();
        let urls: Vec<_> = vm.take_commands(|_| true).collect();
        assert_eq!(urls, [HostCommand::OpenUrl("https://twitter.com/RWSstudios".to_owned()), HostCommand::OpenUrl("steam://store/707030".to_owned())]);
    }
}
