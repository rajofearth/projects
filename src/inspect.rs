//! `detect::inspect` for every project, on one background thread, cached by project name.
//! The home cards ask for everything once a scan lands (descriptions and dirty counts fill
//! in as they arrive); the detail panel asks for its project first and refreshes it when the
//! cached answer is older than half a minute.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rune::Waker;

use crate::detect::{self, Info};

type Queue = Arc<(Mutex<VecDeque<(String, PathBuf)>>, Condvar)>;

pub struct Inspector {
    info: HashMap<String, (Arc<Info>, Instant)>,
    /// `detect::stack`, read on the UI thread the first time a card needs it (manifests only).
    stacks: HashMap<String, Vec<&'static str>>,
    queue: Queue,
    queued: HashSet<String>,
    tx: Sender<(String, Info)>,
    rx: Receiver<(String, Info)>,
    started: bool,
    /// The demo scene answers from the fixture instead of the disk.
    pub demo: bool,
}

const FRESH: Duration = Duration::from_secs(30);

impl Inspector {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Inspector {
            info: HashMap::new(),
            stacks: HashMap::new(),
            queue: Arc::new((Mutex::new(VecDeque::new()), Condvar::new())),
            queued: HashSet::new(),
            tx,
            rx,
            started: false,
            demo: false,
        }
    }

    pub fn get(&self, name: &str) -> Option<&Info> {
        self.info.get(name).map(|(i, _)| i.as_ref())
    }

    pub fn pending(&self, name: &str) -> bool {
        self.queued.contains(name)
    }

    pub fn stack(&self, name: &str) -> Option<&[&'static str]> {
        self.stacks.get(name).map(|v| v.as_slice())
    }

    pub fn has_stack(&self, name: &str) -> bool {
        self.stacks.contains_key(name)
    }

    pub fn set_stack(&mut self, name: &str, stack: Vec<&'static str>) {
        self.stacks.insert(name.to_string(), stack);
    }

    /// Forget everything: a different root, or a scan that renamed folders.
    pub fn clear(&mut self) {
        self.info.clear();
        self.stacks.clear();
        self.queue.0.lock().unwrap().clear();
        self.queued.clear();
    }

    /// Ask for a project's facts. `urgent` jumps the queue (the detail panel), and also
    /// refreshes an answer older than `FRESH`.
    pub fn want(&mut self, name: &str, path: PathBuf, urgent: bool, waker: Option<&Waker>) {
        let stale = self.info.get(name).is_none_or(|(_, at)| at.elapsed() > FRESH);
        if self.info.contains_key(name) && !(urgent && stale) {
            return;
        }
        if self.demo {
            self.info
                .insert(name.to_string(), (Arc::new(crate::fixture::demo_info(name)), Instant::now()));
            return;
        }
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        if self.queued.contains(name) {
            if urgent {
                if let Some(i) = q.iter().position(|(n, _)| n == name) {
                    let item = q.remove(i).unwrap();
                    q.push_front(item);
                }
            }
            return;
        }
        self.queued.insert(name.to_string());
        if urgent {
            q.push_front((name.to_string(), path));
        } else {
            q.push_back((name.to_string(), path));
        }
        cv.notify_one();
        drop(q);
        if !self.started {
            self.started = true;
            let queue = self.queue.clone();
            let tx = self.tx.clone();
            let waker: Waker = waker.cloned().unwrap_or_else(|| Arc::new(|| ()));
            std::thread::spawn(move || worker(queue, tx, waker));
        }
    }

    /// Answers that arrived since the last frame. True when any did.
    pub fn drain(&mut self) -> bool {
        let mut any = false;
        while let Ok((name, info)) = self.rx.try_recv() {
            self.queued.remove(&name);
            self.stacks.insert(name.clone(), info.stack.clone());
            self.info.insert(name, (Arc::new(info), Instant::now()));
            any = true;
        }
        any
    }

    /// Drop the cached facts for a folder that changed, so `want` re-reads them. A request
    /// already on the queue is dropped too; the scan's row is the one that matters now.
    pub fn invalidate(&mut self, name: &str) {
        self.info.remove(name);
        self.stacks.remove(name);
        self.queued.remove(name);
        let queue = self.queue.0.lock().unwrap();
        let mut queue = queue;
        queue.retain(|(n, _)| n != name);
    }

    /// Restore facts from the disk cache. They are treated as fresh so a startup does not
    /// re-read every description; only folders the scan reports as changed are invalidated.
    pub fn restore(&mut self, entries: Vec<(String, Info)>) {
        for (name, info) in entries {
            self.stacks.insert(name.clone(), info.stack.clone());
            self.info.insert(name, (Arc::new(info), Instant::now()));
        }
    }

    /// Everything already read, except folders whose facts are still in flight. The cache
    /// writer uses this so a half-finished inspection is never persisted as final.
    pub fn snapshot(&self) -> Vec<(String, Info)> {
        self.info
            .iter()
            .filter(|(name, _)| !self.queued.contains(*name))
            .map(|(name, (info, _))| (name.clone(), (**info).clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(desc: &str) -> Info {
        Info {
            description: Some(desc.into()),
            stack: vec!["Rust"],
            ..Info::default()
        }
    }

    #[test]
    fn restored_facts_are_reused_until_invalidated() {
        let mut inspector = Inspector::new();
        inspector.restore(vec![("ply".into(), info("cached"))]);
        assert_eq!(inspector.get("ply").unwrap().description.as_deref(), Some("cached"));
        assert!(inspector.has_stack("ply"));
        // A normal request does not re-read a restored folder.
        inspector.want("ply", PathBuf::from("ply"), false, None);
        assert!(!inspector.pending("ply"));

        inspector.invalidate("ply");
        assert!(inspector.get("ply").is_none(), "a changed folder loses its old facts");
        assert!(!inspector.has_stack("ply"));
        inspector.want("ply", PathBuf::from("ply"), false, None);
        assert!(inspector.pending("ply"), "and is queued for a fresh read");
    }

    #[test]
    fn a_snapshot_leaves_out_facts_still_in_flight() {
        let mut inspector = Inspector::new();
        inspector.restore(vec![("ready".into(), info("ready"))]);
        // A changed folder has no facts yet and a read for it is on the way.
        inspector.want("pending", PathBuf::from("pending"), false, None);
        let names: Vec<String> = inspector.snapshot().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["ready"], "in-flight facts are not written to the cache");
    }
}

fn worker(queue: Queue, tx: Sender<(String, Info)>, waker: Waker) {
    let (lock, cv) = &*queue;
    loop {
        let next = {
            let mut q = lock.lock().unwrap();
            while q.is_empty() {
                q = cv.wait(q).unwrap();
            }
            q.pop_front()
        };
        let Some((name, path)) = next else { continue };
        let info = detect::inspect(&path);
        if tx.send((name, info)).is_err() {
            return;
        }
        waker();
    }
}
