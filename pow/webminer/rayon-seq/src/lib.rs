//! Sequential stand-in for rayon: `par_iter`/`par_iter_mut`/`into_par_iter` are the ordinary
//! iterators (so `.map`, `.enumerate`, `.zip`, `.filter_map`, `.collect` are std's), a thread
//! pool runs closures on the calling thread, and there is one thread.

pub mod prelude {
    pub use crate::{IntoParallelIterator, IntoParallelRefIterator, IntoParallelRefMutIterator, SeqIteratorExt};
}

/// rayon's iterator methods that std's iterators don't have.
pub trait SeqIteratorExt: Iterator + Sized {
    /// One `init()` value for the whole (single-threaded) run, then a map.
    fn map_init<T, INIT, F, R>(self, init: INIT, f: F) -> impl Iterator<Item = R>
    where
        INIT: Fn() -> T,
        F: Fn(&mut T, Self::Item) -> R,
    {
        let mut t = init();
        self.map(move |x| f(&mut t, x))
    }
    fn with_min_len(self, _: usize) -> Self {
        self
    }
    fn with_max_len(self, _: usize) -> Self {
        self
    }
}
impl<I: Iterator> SeqIteratorExt for I {}

pub trait IntoParallelIterator {
    type Iter: Iterator;
    fn into_par_iter(self) -> Self::Iter;
}
impl<T: IntoIterator> IntoParallelIterator for T {
    type Iter = T::IntoIter;
    fn into_par_iter(self) -> Self::Iter {
        self.into_iter()
    }
}

pub trait IntoParallelRefIterator<'a> {
    type Iter: Iterator;
    fn par_iter(&'a self) -> Self::Iter;
}
impl<'a, T: 'a + ?Sized> IntoParallelRefIterator<'a> for T
where
    &'a T: IntoIterator,
{
    type Iter = <&'a T as IntoIterator>::IntoIter;
    fn par_iter(&'a self) -> Self::Iter {
        self.into_iter()
    }
}

pub trait IntoParallelRefMutIterator<'a> {
    type Iter: Iterator;
    fn par_iter_mut(&'a mut self) -> Self::Iter;
}
impl<'a, T: 'a + ?Sized> IntoParallelRefMutIterator<'a> for T
where
    &'a mut T: IntoIterator,
{
    type Iter = <&'a mut T as IntoIterator>::IntoIter;
    fn par_iter_mut(&'a mut self) -> Self::Iter {
        self.into_iter()
    }
}

pub fn current_num_threads() -> usize {
    1
}
pub fn current_thread_index() -> Option<usize> {
    Some(0)
}
pub fn join<A, B, RA, RB>(a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA,
    B: FnOnce() -> RB,
{
    (a(), b())
}

#[derive(Debug)]
pub struct ThreadPoolBuildError;
impl std::fmt::Display for ThreadPoolBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("thread pool")
    }
}
impl std::error::Error for ThreadPoolBuildError {}

#[derive(Debug, Default)]
pub struct ThreadPool;
impl ThreadPool {
    pub fn install<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }
    pub fn current_num_threads(&self) -> usize {
        1
    }
}

#[derive(Debug, Default)]
pub struct ThreadPoolBuilder;
impl ThreadPoolBuilder {
    pub fn new() -> Self {
        ThreadPoolBuilder
    }
    pub fn num_threads(self, _: usize) -> Self {
        self
    }
    pub fn build(self) -> Result<ThreadPool, ThreadPoolBuildError> {
        Ok(ThreadPool)
    }
    pub fn build_global(self) -> Result<(), ThreadPoolBuildError> {
        Ok(())
    }
}
