//! デコードワーカープールとチャネル（F-1。v1.0 詳細 §4.1 / F-1 提案）。
//!
//! - 表示用の高優先キューと先読み用の通常キューを分け、ワーカーは `select_biased!` で
//!   高優先を優先して受信する。
//! - epoch（候補列世代）を共有し、デコード前に古い要求を破棄する。
//! - RAM はバイト予約せず、F-8（巨大画像の退避）で寸法上限以下に限定して有界化する。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

use crate::image_loader::{self, DecodeError, DecodedImage, FileFingerprint};

/// デコード要求。投入時点の候補列世代とファイル指紋を保持する。
pub struct DecodeRequest {
    pub path: PathBuf,
    pub epoch: u64,
    pub fingerprint: Option<FileFingerprint>,
}

/// ワーカーの返す結果種別。
pub enum DecodeOutcome {
    Ok(DecodedImage),
    Failed,
    Missing,
    /// 寸法上限超過（F-8）。w/h は将来の閾値ログ用に保持（現状 app は path で退避）。
    Oversized {
        #[allow(dead_code)]
        w: u32,
        #[allow(dead_code)]
        h: u32,
    },
}

impl From<Result<DecodedImage, DecodeError>> for DecodeOutcome {
    fn from(r: Result<DecodedImage, DecodeError>) -> Self {
        match r {
            Ok(img) => DecodeOutcome::Ok(img),
            Err(DecodeError::Missing) => DecodeOutcome::Missing,
            Err(DecodeError::Failed) => DecodeOutcome::Failed,
            Err(DecodeError::Oversized { w, h }) => DecodeOutcome::Oversized { w, h },
        }
    }
}

pub struct DecodeResult {
    pub path: PathBuf,
    pub epoch: u64,
    pub fingerprint: Option<FileFingerprint>,
    pub outcome: DecodeOutcome,
}

pub struct DecoderPool {
    tx_priority: Option<Sender<DecodeRequest>>,
    tx_prefetch: Option<Sender<DecodeRequest>>,
    rx_res: Option<Receiver<DecodeResult>>,
    current_epoch: Arc<AtomicU64>,
    workers: Option<Vec<JoinHandle<()>>>,
}

fn resolve_worker_count(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    cores.saturating_sub(1).clamp(1, 4)
}

impl DecoderPool {
    pub fn new(ctx: egui::Context, workers: usize) -> Self {
        let k = resolve_worker_count(workers);
        let (tx_priority, rx_priority) = bounded::<DecodeRequest>(crate::DISPLAY_QUEUE_CAPACITY);
        let (tx_prefetch, rx_prefetch) = bounded::<DecodeRequest>(crate::PREFETCH_QUEUE_CAPACITY);
        let (tx_res, rx_res) = bounded::<DecodeResult>(crate::RESULT_QUEUE_CAPACITY);
        let current_epoch = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::with_capacity(k);
        for _ in 0..k {
            let rx_p = rx_priority.clone();
            let rx_f = rx_prefetch.clone();
            let tx_r = tx_res.clone();
            let epoch = current_epoch.clone();
            let ctx = ctx.clone();
            let handle = std::thread::spawn(move || {
                worker_loop(rx_p, rx_f, tx_r, epoch, ctx);
            });
            handles.push(handle);
        }

        Self {
            tx_priority: Some(tx_priority),
            tx_prefetch: Some(tx_prefetch),
            rx_res: Some(rx_res),
            current_epoch,
            workers: Some(handles),
        }
    }

    /// 表示枠用要求（高優先）。満杯でもブロックしない。
    pub fn request_display(&self, req: DecodeRequest) -> Result<(), TrySendError<DecodeRequest>> {
        match &self.tx_priority {
            Some(tx) => tx.try_send(req),
            None => Err(TrySendError::Disconnected(req)),
        }
    }

    /// 先読み用要求（通常）。満杯でもブロックしない。
    pub fn request_prefetch(&self, req: DecodeRequest) -> Result<(), TrySendError<DecodeRequest>> {
        match &self.tx_prefetch {
            Some(tx) => tx.try_send(req),
            None => Err(TrySendError::Disconnected(req)),
        }
    }

    /// 非ブロッキングに結果を1件取り出す。
    pub fn try_recv(&self) -> Option<DecodeResult> {
        self.rx_res.as_ref().and_then(|r| r.try_recv().ok())
    }

    /// 現在の候補列世代をワーカーへ通知する。
    pub fn set_epoch(&self, epoch: u64) {
        self.current_epoch.store(epoch, Ordering::SeqCst);
    }
}

impl Drop for DecoderPool {
    fn drop(&mut self) {
        // 要求 Sender と結果 Receiver を先に切断し、結果送信待ちのワーカーを解放する。
        self.tx_priority = None;
        self.tx_prefetch = None;
        self.rx_res = None;
        if let Some(workers) = self.workers.take() {
            for w in workers {
                let _ = w.join();
            }
        }
    }
}

fn worker_loop(
    rx_priority: Receiver<DecodeRequest>,
    rx_prefetch: Receiver<DecodeRequest>,
    tx_res: Sender<DecodeResult>,
    current_epoch: Arc<AtomicU64>,
    ctx: egui::Context,
) {
    loop {
        // 高優先キューを優先して待機。両方とも切断されたら終了。
        let req = crossbeam_channel::select_biased! {
            recv(rx_priority) -> msg => match msg { Ok(r) => r, Err(_) => {
                // 高優先が切断。先読みだけ残っていれば処理を続ける。
                match rx_prefetch.recv() { Ok(r) => r, Err(_) => break }
            }},
            recv(rx_prefetch) -> msg => match msg { Ok(r) => r, Err(_) => {
                match rx_priority.recv() { Ok(r) => r, Err(_) => break }
            }},
        };

        // デコード前の世代失効チェック（古い要求はデコードせず破棄）。
        if req.epoch != current_epoch.load(Ordering::SeqCst) {
            continue;
        }

        let outcome: DecodeOutcome = image_loader::decode_color(&req.path).into();
        let result = DecodeResult {
            path: req.path,
            epoch: req.epoch,
            fingerprint: req.fingerprint,
            outcome,
        };
        // 結果送信。Receiver 切断（UI 終了）なら送信失敗 → ワーカーは次ループで終了判定。
        if tx_res.send(result).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}
