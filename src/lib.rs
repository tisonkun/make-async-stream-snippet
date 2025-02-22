#![feature(unboxed_closures)]
#![feature(async_fn_traits)]

use std::cell::Cell;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr;
use std::task::Context;
use std::task::Poll;

use futures_core::stream::FusedStream;
use futures_core::stream::Stream;

pub fn make_stream<T>(
    closure: impl AsyncFnOnce(&mut Sender<T>) -> () + 'static,
) -> impl Stream<Item = T> {
    let (mut tx, rx) = pair::<T>();
    AsyncStream::new(rx, async move {
        closure.async_call_once((&mut tx,)).await;
    })
}

pub fn make_try_stream<T, E>(
    closure: impl AsyncFnOnce(&mut TrySender<T, E>) -> Result<(), E> + 'static,
) -> impl Stream<Item = Result<T, E>> {
    let (tx, rx) = pair::<Result<T, E>>();
    let mut tx = TrySender { sender: tx };
    AsyncStream::new(rx, async move {
        let result = closure.async_call_once((&mut tx,)).await;
        if let Err(err) = result {
            tx.sender.send(Err(err)).await;
        }
    })
}

#[pin_project::pin_project]
#[derive(Debug)]
pub struct AsyncStream<T, U> {
    rx: Receiver<T>,
    done: bool,
    #[pin]
    generator: U,
}

impl<T, U> AsyncStream<T, U> {
    fn new(rx: Receiver<T>, generator: U) -> AsyncStream<T, U> {
        AsyncStream {
            rx,
            done: false,
            generator,
        }
    }
}

impl<T, U> FusedStream for AsyncStream<T, U>
where
    U: Future<Output = ()>,
{
    fn is_terminated(&self) -> bool {
        self.done
    }
}

impl<T, U> Stream for AsyncStream<T, U>
where
    U: Future<Output = ()>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let me = self.project();

        if *me.done {
            return Poll::Ready(None);
        }

        let mut dst = None;
        let res = {
            let _enter = me.rx.enter(&mut dst);
            me.generator.poll(cx)
        };

        *me.done = res.is_ready();

        if dst.is_some() {
            return Poll::Ready(dst.take());
        }

        if *me.done {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.done {
            (0, Some(0))
        } else {
            (0, None)
        }
    }
}

thread_local!(static STORE: Cell<*mut ()> = const { Cell::new(ptr::null_mut()) });

fn pair<T>() -> (Sender<T>, Receiver<T>) {
    let tx = Sender { p: PhantomData };
    let rx = Receiver { p: PhantomData };
    (tx, rx)
}

#[derive(Debug)]
pub struct TrySender<T, E> {
    sender: Sender<Result<T, E>>,
}

impl<T, E> TrySender<T, E> {
    pub fn send(&mut self, value: T) -> impl Future<Output = ()> {
        Send {
            value: Some(Ok::<T, E>(value)),
        }
    }
}

#[derive(Debug)]
pub struct Sender<T> {
    p: PhantomData<fn(T) -> T>,
}

impl<T> Sender<T> {
    pub fn send(&mut self, value: T) -> impl Future<Output = ()> {
        Send { value: Some(value) }
    }
}

struct Send<T> {
    value: Option<T>,
}

impl<T> Unpin for Send<T> {}

impl<T> Future for Send<T> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.value.is_none() {
            return Poll::Ready(());
        }

        STORE.with(|cell| {
            let ptr = cell.get() as *mut Option<T>;
            #[allow(unsafe_code)]
            let option_ref = unsafe { ptr.as_mut() }.expect("invalid usage");

            if option_ref.is_none() {
                *option_ref = self.value.take();
            }

            Poll::Pending
        })
    }
}

#[derive(Debug)]
struct Receiver<T> {
    p: PhantomData<T>,
}

struct Enter<'a, T> {
    prev: *mut (),
    #[expect(unused)]
    rx: &'a mut Receiver<T>,
}

impl<T> Receiver<T> {
    pub(crate) fn enter<'a>(&'a mut self, dst: &'a mut Option<T>) -> Enter<'a, T> {
        let prev = STORE.with(|cell| {
            let prev = cell.get();
            cell.set(dst as *mut _ as *mut ());
            prev
        });

        Enter { rx: self, prev }
    }
}

impl<T> Drop for Enter<'_, T> {
    fn drop(&mut self) {
        STORE.with(|cell| cell.set(self.prev));
    }
}
