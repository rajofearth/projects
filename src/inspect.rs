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
