//! 設定＋サイクル状態のディスク保存／復元（F-7。v1.0 詳細 §3.2 / §4.7.4）。

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::{PERSIST_SCHEMA_VERSION, STATE_FILE_NAME};

#[derive(Serialize, Deserialize, Clone)]
pub struct PersistState {
    pub schema_version: u32,
    pub folder: PathBuf,
    pub interval_secs: u64,
    pub display_count: usize,
    pub play_order: Vec<PathBuf>,
    pub cursor: usize,
    pub cycle_count: u32,
    #[serde(default)]
    pub last_shown: Vec<PathBuf>,
}

/// 保存先候補（優先順）: 実行ファイル隣 → `%APPDATA%\ShufflePic` → カレントディレクトリ。
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("ShufflePic"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    dirs
}

/// 既存の状態ファイルを読み、最も新しいものを採用する（無ければ None）。
/// 採用したファイルパスも返し、以後の保存先として一貫使用する（v1.0 詳細 §4.7.4）。
pub fn load() -> Option<(PathBuf, PersistState)> {
    let mut best: Option<(std::time::SystemTime, PathBuf, PersistState)> = None;
    for dir in candidate_dirs() {
        let file = dir.join(STATE_FILE_NAME);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let state: PersistState = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if state.schema_version != PERSIST_SCHEMA_VERSION {
            continue;
        }
        let mtime = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        match &best {
            Some((bt, _, _)) if *bt >= mtime => {}
            _ => best = Some((mtime, file.clone(), state)),
        }
    }
    best.map(|(_, p, s)| (p, s))
}

/// 状態を保存する。一時ファイルへ全量書き込み → `sync_all` → `rename` で置換。
/// `chosen` が未確定なら書き込み可能な候補を選んで固定する。
pub fn save(chosen: &mut Option<PathBuf>, state: &PersistState) -> io::Result<()> {
    if chosen.is_none() {
        for dir in candidate_dirs() {
            if std::fs::create_dir_all(&dir).is_ok() {
                // 書き込み可能性の簡易確認。
                let probe = dir.join(".shufflepic_write_probe");
                if std::fs::write(&probe, b"x").is_ok() {
                    let _ = std::fs::remove_file(&probe);
                    *chosen = Some(dir.join(STATE_FILE_NAME));
                    break;
                }
            }
        }
    }
    let path = chosen
        .clone()
        .ok_or_else(|| io::Error::other("no writable location for state file"))?;
    write_atomic(&path, state)
}

fn write_atomic(path: &Path, state: &PersistState) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.flush()?;
        f.sync_all()?;
    }
    // Windows 10 1607+/Win11 では rename が既存ファイルを置換する（v1.0 詳細 §4.7.4）。
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// 復元時の突き合わせ（v1.0 詳細 §4.7.4）。
/// 保存 play_order を「表示済み／未表示」に分け、指定フォルダ直下の有効パスだけ採用。
/// 戻り値: (play_order, cursor, last_shown)。cycle_count は呼び出し側で別途復元する。
pub fn reconcile(state: &PersistState, scanned: &[PathBuf]) -> (Vec<PathBuf>, usize, Vec<PathBuf>) {
    let scanned_set: HashSet<&PathBuf> = scanned.iter().collect();
    let safe_cursor = state.cursor.min(state.play_order.len());

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut valid_shown: Vec<PathBuf> = Vec::new();
    for p in &state.play_order[..safe_cursor] {
        if scanned_set.contains(p) && seen.insert(p.clone()) {
            valid_shown.push(p.clone());
        }
    }
    let mut valid_unshown: Vec<PathBuf> = Vec::new();
    for p in &state.play_order[safe_cursor..] {
        if scanned_set.contains(p) && seen.insert(p.clone()) {
            valid_unshown.push(p.clone());
        }
    }

    // 実スキャンにのみ存在する新規パスを未表示側へ追加（シャッフル）。
    let mut new_paths: Vec<PathBuf> = scanned
        .iter()
        .filter(|p| !seen.contains(*p))
        .cloned()
        .collect();
    new_paths.shuffle(&mut rand::thread_rng());

    let cursor = valid_shown.len();
    let mut play_order = valid_shown;
    play_order.extend(valid_unshown);
    play_order.extend(new_paths);

    let last_shown: Vec<PathBuf> = state
        .last_shown
        .iter()
        .filter(|p| scanned_set.contains(*p))
        .take(2)
        .cloned()
        .collect();

    (play_order, cursor, last_shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_temp(tag: &str) -> PathBuf {
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sp_p_{tag}_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_read_round_trip() {
        let dir = unique_temp("save");
        let state = PersistState {
            schema_version: PERSIST_SCHEMA_VERSION,
            folder: dir.clone(),
            interval_secs: 23,
            display_count: 1,
            play_order: vec![dir.join("a.png"), dir.join("b.png")],
            cursor: 1,
            cycle_count: 4,
            last_shown: vec![dir.join("a.png")],
        };
        let mut chosen = Some(dir.join(STATE_FILE_NAME));
        save(&mut chosen, &state).unwrap();

        let bytes = std::fs::read(dir.join(STATE_FILE_NAME)).unwrap();
        let back: PersistState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.schema_version, PERSIST_SCHEMA_VERSION);
        assert_eq!(back.interval_secs, 23);
        assert_eq!(back.display_count, 1);
        assert_eq!(back.cursor, 1);
        assert_eq!(back.cycle_count, 4);
        assert_eq!(back.play_order, state.play_order);
        assert_eq!(back.last_shown, state.last_shown);
        // 一時ファイルが残っていない。
        assert!(!dir.join("shufflepic_state.json.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reconcile_filters_dedups_and_recomputes_cursor() {
        let folder = PathBuf::from("/some/folder");
        let a = folder.join("a.png");
        let b = folder.join("b.png");
        let c = folder.join("c.png");
        let d = folder.join("d.png");
        let outside = PathBuf::from("/other/x.png");

        let state = PersistState {
            schema_version: PERSIST_SCHEMA_VERSION,
            folder: folder.clone(),
            interval_secs: 15,
            display_count: 2,
            // 表示済み = [a, outside, b]、未表示 = [a(dup), c]
            play_order: vec![
                a.clone(),
                outside.clone(),
                b.clone(),
                a.clone(),
                c.clone(),
            ],
            cursor: 3,
            cycle_count: 2,
            last_shown: vec![a.clone(), outside.clone()],
        };
        // 実スキャン: a,b,c + 新規 d（outside は存在しない）。
        let scanned = vec![a.clone(), b.clone(), c.clone(), d.clone()];

        let (order, cursor, last_shown) = reconcile(&state, &scanned);

        // 有効表示済み = [a, b]（outside 除外）→ cursor=2。
        assert_eq!(cursor, 2);
        // 先頭3件 = 有効表示済み[a,b] + 有効未表示[c]。
        assert_eq!(&order[..3], &[a.clone(), b.clone(), c.clone()][..]);
        // 新規 d は末尾側に含まれ、フォルダ外 outside と重複 a は除外。
        assert!(order.contains(&d));
        assert!(!order.contains(&outside));
        assert_eq!(order.iter().filter(|p| **p == a).count(), 1, "dedup");
        // last_shown はフォルダ内のみ（outside 除外）。
        assert_eq!(last_shown, vec![a]);
    }
}
