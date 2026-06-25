//! 隔離移動（`delete` 退避・F-8 `oversized` 退避）の共通処理（v1.0 詳細 §4.3 / §4.8）。
//! v1.0 では Undo を廃止（F-3）したため履歴・復元は持たない。

use std::io;
use std::path::{Path, PathBuf};

/// 移動先の衝突しないファイル名を生成する。
/// `stem`, `stem (1).ext`, `stem (2).ext` … の順。
fn candidate_name(dir: &Path, file_name: &Path, n: u32) -> PathBuf {
    if n == 0 {
        return dir.join(file_name);
    }
    let stem = file_name
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = file_name.extension().and_then(|e| e.to_str());
    let new_name = match ext {
        Some(ext) => format!("{stem} ({n}).{ext}"),
        None => format!("{stem} ({n})"),
    };
    dir.join(new_name)
}

/// `dir`（`delete` または `oversized`）へ衝突回避で移動する（共通ヘルパ）。
/// `rename` の上書き仕様には依存せず、衝突回避＋AlreadyExists リトライで安全側に倒す。
/// 成功時は移動先パスを返す。
pub fn move_with_collision_avoidance(src: &Path, dir: &Path) -> io::Result<PathBuf> {
    let file_name = match src.file_name() {
        Some(n) => PathBuf::from(n),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source has no file name",
            ))
        }
    };

    // 第一手段: ハードリンク＋原本削除（同一FS）。`hard_link` は宛先が既存なら AlreadyExists で
    // 必ず失敗するため、既存ファイルを上書きしない（rename 置換の TOCTOU を回避）。
    let mut n: u32 = 0;
    loop {
        if n > 1_000_000 {
            return Err(io::Error::other("could not find a free quarantine name"));
        }
        let dest = candidate_name(dir, &file_name, n);
        match std::fs::hard_link(src, &dest) {
            Ok(()) => {
                // リンク作成成功＝宛先を原子的に確保。原本リンクを削除して移動完了。
                match std::fs::remove_file(src) {
                    Ok(()) => return Ok(dest),
                    Err(e) => {
                        // 原本を消せなかった → 複製を残さないよう作成したリンクを撤去。
                        let _ = std::fs::remove_file(&dest);
                        return Err(e);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                n += 1;
                continue;
            }
            // ハードリンク非対応FS（FAT/exFAT 等）・異FS など → 予約+rename へフォールバック。
            Err(_) => break,
        }
    }

    // フォールバック（リンク非対応FS = FAT/exFAT 等／異FS）:
    // `create_new` で宛先名を原子的に予約し、開いたままのハンドルへ**内容をコピー**してから原本を削除する。
    // rename の上書き置換を使わないため、既存ファイルや他プロセスが置いたファイルを上書きしない。
    loop {
        if n > 1_000_000 {
            return Err(io::Error::other("could not find a free quarantine name"));
        }
        let dest = candidate_name(dir, &file_name, n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(mut out) => {
                // 予約したファイルへ内容をコピー（自前のハンドルへ書くので上書き競合は起きない）。
                let copy_res = (|| -> io::Result<()> {
                    let mut input = std::fs::File::open(src)?;
                    std::io::copy(&mut input, &mut out)?;
                    out.sync_all()?;
                    Ok(())
                })();
                match copy_res {
                    Ok(()) => {
                        drop(out);
                        // コピー成功後に原本を削除＝移動完了。原本削除に失敗したら複製を残さない。
                        match std::fs::remove_file(src) {
                            Ok(()) => return Ok(dest),
                            Err(e) => {
                                let _ = std::fs::remove_file(&dest);
                                return Err(e);
                            }
                        }
                    }
                    Err(e) => {
                        drop(out);
                        let _ = std::fs::remove_file(&dest); // 中途半端な複製を掃除（原本は無傷）
                        return Err(e);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                n += 1;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

/// `<base>/<name>` の退避フォルダを準備する。
/// - 既にファイルとして存在 → エラー。
/// - 無ければ作成。ディレクトリなら何もしない。
pub fn prepare_dir(base: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = base.join(name);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.is_dir() {
                Ok(dir)
            } else {
                Err(format!(
                    "退避フォルダのパスが既にファイルとして存在します（上書きしません）: {}",
                    dir.display()
                ))
            }
        }
        Err(_) => match std::fs::create_dir(&dir) {
            Ok(()) => Ok(dir),
            Err(e) => Err(format!("退避フォルダの作成に失敗しました: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_temp(tag: &str) -> PathBuf {
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("shufflepic_q_{tag}_{}_{n}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn move_basic() {
        let base = unique_temp("basic");
        let src = base.join("a.png");
        fs::write(&src, b"DATA").unwrap();
        let dir = base.join("delete");
        fs::create_dir_all(&dir).unwrap();

        let dest = move_with_collision_avoidance(&src, &dir).unwrap();
        assert!(!src.exists(), "original should be moved away");
        assert_eq!(dest.file_name().unwrap(), "a.png");
        assert_eq!(fs::read(&dest).unwrap(), b"DATA");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn move_collision_does_not_overwrite() {
        let base = unique_temp("collision");
        let src = base.join("a.png");
        fs::write(&src, b"NEW").unwrap();
        let dir = base.join("delete");
        fs::create_dir_all(&dir).unwrap();
        let dummy = dir.join("a.png");
        fs::write(&dummy, b"DUMMY").unwrap();

        let dest = move_with_collision_avoidance(&src, &dir).unwrap();
        assert_eq!(dest.file_name().unwrap(), "a (1).png");
        assert_eq!(fs::read(&dummy).unwrap(), b"DUMMY");
        assert_eq!(fs::read(&dest).unwrap(), b"NEW");

        fs::remove_dir_all(&base).ok();
    }
}
