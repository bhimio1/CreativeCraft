//! Marketplace audio plugins: CLAP / VST3 bundles from the audio plugin index. Installing
//! downloads the publisher's latest GitHub release for this computer (checked against its
//! published SHA-256 sums), finds `<bundle>.clap` / `<bundle>.vst3` in it and copies them into the
//! user's own plugin folders, where music apps (DAWs) look for them. The exact paths are recorded
//! in the settings, and removing deletes exactly those. Audio plugins have no MCP server: agents
//! never get tools from them.

use crate::{EngineError, Event, PluginState, Session};
use creativecraft_install::{Progress, Release};
use creativecraft_marketplace::{AudioIndex, AudioPlugin, Format, Index};
use creativecraft_platform::{Host, Os};
use std::path::{Path, PathBuf};

/// Put both formats under one folder instead (`<dir>/clap`, `<dir>/vst3`): tests and portable
/// setups.
pub const DIR_OVERRIDE: &str = "CREATIVECRAFT_AUDIO_PLUGIN_DIR";

/// What the plugin folders depend on, read from the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginDirEnv {
    pub home: Option<PathBuf>,
    /// `%LOCALAPPDATA%` (Windows).
    pub local_app_data: Option<PathBuf>,
    /// [`DIR_OVERRIDE`].
    pub override_dir: Option<PathBuf>,
}

impl PluginDirEnv {
    /// This process's environment. Inside the Flatpak `HOME` is still the real home folder (the
    /// sandbox has home access), so plugins land where the host's DAWs look.
    pub fn current() -> PluginDirEnv {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        PluginDirEnv { home: var("HOME").or_else(|| var("USERPROFILE")), local_app_data: var("LOCALAPPDATA"), override_dir: var(DIR_OVERRIDE) }
    }
}

/// The per-user folder DAWs scan for `format` plugins on `os` (no admin rights needed).
pub fn audio_plugin_dir(os: Os, format: Format, env: &PluginDirEnv) -> Option<PathBuf> {
    if let Some(dir) = &env.override_dir {
        return Some(dir.join(format.extension()));
    }
    let upper = format.label();
    match os {
        Os::Linux | Os::FreeBsd => env.home.as_ref().map(|h| h.join(format!(".{}", format.extension()))),
        Os::MacOs => env.home.as_ref().map(|h| h.join("Library").join("Audio").join("Plug-Ins").join(upper)),
        Os::Windows => env.local_app_data.as_ref().map(|l| l.join("Programs").join("Common").join(upper)),
    }
}

/// The plugin folders for every format on this computer.
fn all_dirs(os: Os, env: &PluginDirEnv) -> Vec<PathBuf> {
    Format::ALL.iter().filter_map(|f| audio_plugin_dir(os, *f, env)).collect()
}

/// `/home/me/.clap` → `~/.clap`, for messages.
fn show_dir(dir: &Path, env: &PluginDirEnv) -> String {
    match env.home.as_ref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if env.override_dir.is_none() => format!("~/{}", rest.display()),
        _ => dir.display().to_string(),
    }
}

/// What installing `plugin` on this computer does, in a sentence, for the confirmation:
/// "Downloads MusicCraft-Plugins for this computer from github.com/bhimio1/musiccraft releases and
/// copies MusicCraft Effects.clap and .vst3 into ~/.clap and ~/.vst3."
pub fn audio_install_plan(plugin: &AudioPlugin) -> String {
    let env = PluginDirEnv::current();
    let os = Host::current().map(|h| h.os);
    let exts: Vec<String> = plugin.formats.iter().map(|f| format!(".{}", f.extension())).collect();
    let dirs: Vec<String> = plugin
        .formats
        .iter()
        .map(|f| os.and_then(|os| audio_plugin_dir(os, *f, &env)).map_or_else(|| format!("this computer's {} folder", f.label()), |d| show_dir(&d, &env)))
        .collect();
    format!(
        "Downloads {} for this computer from github.com/{} releases and copies {}{} into {}.",
        plugin.asset,
        plugin.repo,
        plugin.bundle,
        exts.join(" and "),
        dirs.join(" and ")
    )
}

/// The asset `prefix` names that is built for `host`, among a release's `names`. The prefix must
/// be followed by a separator, so `MusicCraft-Plugins` never matches the app's own
/// `MusicCraft-0.3.0-…` archives.
pub fn pick_plugin_asset<'a>(names: &[&'a str], prefix: &str, host: Host) -> Option<&'a str> {
    let ours: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            n.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
                && n.get(prefix.len()..).is_some_and(|rest| rest.starts_with(['-', '_', '.']))
        })
        .collect();
    creativecraft_platform::pick_asset(&ours, host)
}

/// Find a bundle (`name`, a file or a folder) anywhere under `root`, preferring the shallowest.
/// Symlinks are not followed, and a found bundle is not looked into.
fn find_bundle(root: &Path, name: &str) -> Option<PathBuf> {
    let mut frontier = vec![root.to_path_buf()];
    for _depth in 0..6 {
        let mut next = Vec::new();
        for dir in &frontier {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let Ok(ft) = e.file_type() else { continue };
                if (ft.is_file() || ft.is_dir()) && e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
                    return Some(e.path());
                }
                if ft.is_dir() {
                    next.push(e.path());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

/// Remove a file, folder or symlink; a missing one is fine.
fn remove_any(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Copy a file or a folder (bundles are folders on macOS, and `.vst3` always), keeping symlinks
/// as symlinks where the OS can.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(std::fs::read_link(src)?, dst);
        #[cfg(not(unix))]
        return copy_tree(&std::fs::canonicalize(src)?, dst);
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(src, dst).map(|_| ())
}

/// Copy `src` into `dir` as `name`: into a hidden temp sibling first, then swapped in, so an older
/// copy is replaced only once the new one is complete (and put back if the swap fails).
fn place(src: &Path, dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(name);
    // names DAWs ignore while scanning: they don't end in .clap / .vst3
    let tmp = dir.join(format!(".{name}.creativecraft-new"));
    let old = dir.join(format!(".{name}.creativecraft-old"));
    remove_any(&tmp)?;
    if let Err(e) = copy_tree(src, &tmp) {
        let _ = remove_any(&tmp);
        return Err(e);
    }
    remove_any(&old)?;
    let had = std::fs::symlink_metadata(&dest).is_ok();
    if had && let Err(e) = std::fs::rename(&dest, &old) {
        let _ = remove_any(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        if had {
            let _ = std::fs::rename(&old, &dest);
        }
        let _ = remove_any(&tmp);
        return Err(e);
    }
    let _ = remove_any(&old);
    Ok(dest)
}

/// Find every format's bundle in `unpacked` (all of them, before anything is copied), then copy
/// each into its folder. Returns the installed paths.
fn install_bundles(plugin: &AudioPlugin, unpacked: &Path, targets: &[(Format, PathBuf)]) -> Result<Vec<PathBuf>, EngineError> {
    let mut found = Vec::new();
    for (format, dir) in targets {
        let name = plugin.bundle_name(*format);
        let src = find_bundle(unpacked, &name).ok_or_else(|| EngineError::Plugin(format!("the download has no {name} in it")))?;
        found.push((src, dir, name));
    }
    let mut placed = Vec::new();
    for (src, dir, name) in found {
        placed.push(place(&src, dir, &name).map_err(|e| EngineError::Plugin(format!("couldn't copy {name} into {}: {e}", dir.display())))?);
    }
    Ok(placed)
}

/// Delete the recorded `files` that are bundles directly inside one of the plugin folders `allowed`
/// (anything else is left alone). Returns what couldn't or mustn't be deleted.
fn remove_recorded(files: &[PathBuf], allowed: &[PathBuf]) -> Vec<PathBuf> {
    let mut kept = Vec::new();
    for f in files {
        let ours = f.parent().is_some_and(|p| allowed.iter().any(|a| a == p)) && f.file_name().and_then(|n| n.to_str()).and_then(Format::of).is_some();
        if !ours || remove_any(f).is_err() {
            kept.push(f.clone());
        }
    }
    kept
}

/// The built-in audio index, or a fetched copy at least as new whose ids don't clash with `mcp`.
pub(crate) fn load_audio_index(cache: &Path, mcp: &Index) -> AudioIndex {
    let builtin = AudioIndex::builtin().unwrap_or(AudioIndex { version: 0, plugins: Vec::new() });
    match std::fs::read_to_string(cache).ok().and_then(|t| AudioIndex::parse(&t).ok()) {
        Some(fetched) if fetched.version >= builtin.version && creativecraft_marketplace::check_ids_unique(mcp, &fetched).is_ok() => fetched,
        _ => builtin,
    }
}

impl Session {
    pub fn audio_plugin(&self, id: &str) -> Result<AudioPlugin, EngineError> {
        self.audio_plugins.get(id).cloned().ok_or_else(|| EngineError::Plugin(format!("no plugin called {id}")))
    }

    /// Whether `id` is an audio plugin: listed as one, or installed as one (with recorded files).
    pub fn is_audio_plugin(&self, id: &str) -> bool {
        self.audio_plugins.get(id).is_some() || self.settings.plugins.get(id).is_some_and(|s| !s.files.is_empty())
    }

    /// Where each of `plugin`'s formats goes on this computer.
    pub fn audio_plugin_targets(&self, plugin: &AudioPlugin) -> Result<Vec<(Format, PathBuf)>, EngineError> {
        let env = PluginDirEnv::current();
        plugin
            .formats
            .iter()
            .map(|f| {
                audio_plugin_dir(self.installer.host.os, *f, &env)
                    .map(|d| (*f, d))
                    .ok_or_else(|| EngineError::Plugin(format!("couldn't find this computer's {} plugin folder", f.label())))
            })
            .collect()
    }

    pub(crate) fn install_audio_plugin(&mut self, id: &str, events: &mut dyn FnMut(Event)) -> Result<String, EngineError> {
        let plugin = self.audio_plugin(id)?;
        let targets = self.audio_plugin_targets(&plugin)?;
        let mut progress = |p: Progress| events(Event::Install(id.to_string(), p));
        progress(Progress::Resolving);
        let release = creativecraft_install::latest_release(&plugin.repo)?;
        let previous = self.settings.plugins.get(id).cloned();
        if let Some(st) = &previous
            && st.version.as_deref() == Some(release.tag.as_str())
            && !st.files.is_empty()
            && st.files.iter().all(|f| std::fs::symlink_metadata(f).is_ok())
        {
            progress(Progress::Done);
            return Ok(format!("{} {} is installed and up to date", plugin.name, crate::updates::show_version(&release.tag)));
        }
        let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        let picked = pick_plugin_asset(&names, &plugin.asset, self.installer.host)
            .ok_or_else(|| {
                EngineError::Plugin(format!("{} has no {} download for this computer in {} {}", plugin.name, plugin.asset, plugin.repo, release.tag))
            })?
            .to_string();

        let staging = self.dirs.data().join("marketplace").join("staging").join(id);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let result = fetch_and_place(self, &plugin, &release, &picked, &staging, &targets, &mut progress);
        let _ = std::fs::remove_dir_all(&staging);
        let files = result?;

        // an older install's bundles that this one didn't replace (a format dropped) go too
        if let Some(old) = previous {
            let stale: Vec<PathBuf> = old.files.into_iter().filter(|f| !files.contains(f)).collect();
            remove_recorded(&stale, &all_dirs(self.installer.host.os, &PluginDirEnv::current()));
        }
        self.settings.plugins.insert(id.to_string(), PluginState { enabled: false, version: Some(release.tag.clone()), files });
        self.save_settings()?;
        progress(Progress::Done);
        let env = PluginDirEnv::current();
        let dirs: Vec<String> = targets.iter().map(|(_, d)| show_dir(d, &env)).collect();
        let notes = if plugin.notes.is_empty() { String::new() } else { format!(" {}", plugin.notes) };
        Ok(format!("{} {} installed into {}.{notes}", plugin.name, crate::updates::show_version(&release.tag), dirs.join(" and ")))
    }

    pub(crate) fn remove_audio_plugin(&mut self, id: &str) -> Result<String, EngineError> {
        let name = self.audio_plugins.get(id).map_or(id.to_string(), |p| p.name.clone());
        let state = self.settings.plugins.get(id).cloned().ok_or_else(|| EngineError::Plugin(format!("{name} isn't installed")))?;
        let kept = remove_recorded(&state.files, &all_dirs(self.installer.host.os, &PluginDirEnv::current()));
        self.settings.plugins.remove(id);
        self.save_settings()?;
        if kept.is_empty() {
            return Ok(format!("{name} removed. Rescan plugins in your music app."));
        }
        let left: Vec<String> = kept.iter().map(|p| p.display().to_string()).collect();
        Ok(format!("{name} removed; left in place (not in a plugin folder, or couldn't be deleted): {}", left.join(", ")))
    }
}

fn fetch_and_place(
    s: &Session,
    plugin: &AudioPlugin,
    release: &Release,
    asset: &str,
    staging: &Path,
    targets: &[(Format, PathBuf)],
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<PathBuf>, EngineError> {
    creativecraft_install::fetch_asset(&s.dirs, &plugin.name, release, asset, staging, progress)?;
    install_bundles(plugin, staging, targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use creativecraft_platform::Arch;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-audio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn plugin_folders_per_os() {
        let env = PluginDirEnv { home: Some("/home/me".into()), local_app_data: Some(r"C:\Users\me\AppData\Local".into()), override_dir: None };
        let dir = |os, f| audio_plugin_dir(os, f, &env).unwrap();
        assert_eq!(dir(Os::Linux, Format::Clap), Path::new("/home/me/.clap"));
        assert_eq!(dir(Os::Linux, Format::Vst3), Path::new("/home/me/.vst3"));
        assert_eq!(dir(Os::FreeBsd, Format::Vst3), Path::new("/home/me/.vst3"));
        assert_eq!(dir(Os::MacOs, Format::Clap), Path::new("/home/me/Library/Audio/Plug-Ins/CLAP"));
        assert_eq!(dir(Os::MacOs, Format::Vst3), Path::new("/home/me/Library/Audio/Plug-Ins/VST3"));
        assert_eq!(dir(Os::Windows, Format::Clap), Path::new(r"C:\Users\me\AppData\Local").join("Programs").join("Common").join("CLAP"));
        assert_eq!(dir(Os::Windows, Format::Vst3), Path::new(r"C:\Users\me\AppData\Local").join("Programs").join("Common").join("VST3"));
        assert_eq!(audio_plugin_dir(Os::Windows, Format::Clap, &PluginDirEnv { local_app_data: None, ..env.clone() }), None);
        let over = PluginDirEnv { override_dir: Some("/portable/plugins".into()), ..env.clone() };
        assert_eq!(audio_plugin_dir(Os::Windows, Format::Vst3, &over).unwrap(), Path::new("/portable/plugins/vst3"));
        assert_eq!(audio_plugin_dir(Os::Linux, Format::Clap, &over).unwrap(), Path::new("/portable/plugins/clap"));
        assert_eq!(show_dir(Path::new("/home/me/.clap"), &env), "~/.clap");
    }

    /// The assets of MusicCraft v0.3.0, as published.
    const V030: &[&str] = &[
        "MusicCraft-0.3.0-linux-x86_64-portable.tar.gz",
        "MusicCraft-0.3.0-linux-x86_64.flatpak",
        "MusicCraft-0.3.0-windows-x64-portable.zip",
        "MusicCraft-0.3.0-windows-x64.msi",
        "MusicCraft-0.3.0-windows-x64.wixpdb",
        "MusicCraft-0.3.0-macos-universal.dmg",
        "MusicCraft-Plugins-0.3.0-linux-x86_64.zip",
        "MusicCraft-Plugins-0.3.0-windows-x64.zip",
        "MusicCraft-Plugins-0.3.0-macos-universal.zip",
        "SHA256SUMS.txt",
    ];

    #[test]
    fn picks_the_plugins_archive_for_each_computer() {
        let pick = |os, arch| pick_plugin_asset(V030, "MusicCraft-Plugins", Host { os, arch });
        assert_eq!(pick(Os::Linux, Arch::X86_64), Some("MusicCraft-Plugins-0.3.0-linux-x86_64.zip"));
        assert_eq!(pick(Os::Windows, Arch::X86_64), Some("MusicCraft-Plugins-0.3.0-windows-x64.zip"));
        assert_eq!(pick(Os::MacOs, Arch::Aarch64), Some("MusicCraft-Plugins-0.3.0-macos-universal.zip"));
        assert_eq!(pick(Os::MacOs, Arch::X86_64), Some("MusicCraft-Plugins-0.3.0-macos-universal.zip"));
        assert_eq!(pick(Os::Linux, Arch::Aarch64), None, "no build for it: an error, never the app");
        assert_eq!(
            pick_plugin_asset(V030, "musiccraft-plugins", Host { os: Os::Linux, arch: Arch::X86_64 }),
            Some("MusicCraft-Plugins-0.3.0-linux-x86_64.zip")
        );
        // a prefix only matches whole name parts
        assert_eq!(pick_plugin_asset(V030, "MusicCraft-Plug", Host { os: Os::Linux, arch: Arch::X86_64 }), None);
    }

    #[test]
    fn finds_and_copies_file_and_folder_bundles() {
        let root = temp("copy");
        let unpacked = root.join("unpacked/MusicCraft-Plugins-0.3.0-linux");
        std::fs::create_dir_all(unpacked.join("MusicCraft Effects.vst3/Contents/x86_64-linux")).unwrap();
        std::fs::write(unpacked.join("MusicCraft Effects.vst3/Contents/x86_64-linux/MusicCraft Effects.so"), b"vst3").unwrap();
        std::fs::write(unpacked.join("MusicCraft Effects.clap"), b"clap").unwrap();
        std::fs::write(unpacked.join("MusicCraft Instruments.clap"), b"other").unwrap();
        std::fs::write(unpacked.join("README.txt"), b"readme").unwrap();
        let plugin = AudioIndex::builtin().unwrap().get("musiccraft-effects").unwrap().clone();
        let targets = vec![(Format::Clap, root.join("target/clap")), (Format::Vst3, root.join("target/vst3"))];

        // an older copy is there already: it's replaced
        std::fs::create_dir_all(root.join("target/vst3/MusicCraft Effects.vst3/Contents/stale")).unwrap();
        let files = install_bundles(&plugin, &root.join("unpacked"), &targets).unwrap();
        assert_eq!(files, vec![root.join("target/clap/MusicCraft Effects.clap"), root.join("target/vst3/MusicCraft Effects.vst3")]);
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"clap");
        assert_eq!(std::fs::read(files[1].join("Contents/x86_64-linux/MusicCraft Effects.so")).unwrap(), b"vst3");
        assert!(!files[1].join("Contents/stale").exists(), "the old copy is gone");
        let leftovers: Vec<_> = std::fs::read_dir(root.join("target/vst3")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no temp copies left: {leftovers:?}");
        assert!(!root.join("target/clap/MusicCraft Instruments.clap").exists(), "only this plugin's bundles");

        // a bundle missing from the download fails before anything is copied
        std::fs::remove_dir_all(root.join("target")).unwrap();
        std::fs::remove_file(unpacked.join("MusicCraft Effects.clap")).unwrap();
        assert!(install_bundles(&plugin, &root.join("unpacked"), &targets).unwrap_err().to_string().contains("MusicCraft Effects.clap"));
        assert!(!root.join("target").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_deletes_only_recorded_bundles_in_plugin_folders() {
        let root = temp("remove");
        let clap = root.join("clap");
        let vst3 = root.join("vst3");
        std::fs::create_dir_all(vst3.join("Mine.vst3/Contents")).unwrap();
        std::fs::create_dir_all(&clap).unwrap();
        std::fs::write(clap.join("Mine.clap"), b"").unwrap();
        std::fs::write(clap.join("Someone Else.clap"), b"").unwrap();
        std::fs::write(root.join("Outside.clap"), b"").unwrap();
        std::fs::write(clap.join("notes.txt"), b"").unwrap();
        let recorded = vec![clap.join("Mine.clap"), vst3.join("Mine.vst3"), root.join("Outside.clap"), clap.join("notes.txt"), clap.join("Gone.clap")];
        let kept = remove_recorded(&recorded, &[clap.clone(), vst3.clone()]);
        assert!(!clap.join("Mine.clap").exists() && !vst3.join("Mine.vst3").exists());
        assert!(clap.join("Someone Else.clap").exists(), "not recorded: untouched");
        assert!(root.join("Outside.clap").exists() && clap.join("notes.txt").exists(), "outside a plugin folder / not a bundle: untouched");
        assert_eq!(kept, vec![root.join("Outside.clap"), clap.join("notes.txt")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fetched_audio_index_must_be_new_enough_and_not_clash() {
        let dir = temp("index");
        let cache = dir.join("audio-plugins.toml");
        let mcp = Index::builtin().unwrap();
        let builtin = AudioIndex::builtin().unwrap();
        std::fs::write(&cache, format!("version = {}\n", builtin.version + 1)).unwrap();
        assert!(load_audio_index(&cache, &mcp).plugins.is_empty(), "a newer copy wins");
        std::fs::write(&cache, "version = 0\n").unwrap();
        assert_eq!(load_audio_index(&cache, &mcp).plugins.len(), builtin.plugins.len());
        let clash = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../marketplace/audio-plugins.toml"))
            .unwrap()
            .replace("version = 1", &format!("version = {}", builtin.version + 1))
            .replace("id = \"musiccraft-effects\"", "id = \"blender-mcp\"");
        std::fs::write(&cache, clash).unwrap();
        assert_eq!(load_audio_index(&cache, &mcp).plugins, builtin.plugins, "a copy clashing with the MCP list is ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
