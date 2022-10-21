use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{ready, Context, Poll},
    time::Duration,
};

use futures_util::{future::LocalBoxFuture, FutureExt, Stream};
use tokio::time::{self, MissedTickBehavior};

pub fn poll<'a, F, T, Fut>(period: Duration, f: F) -> Poller<'a, F, T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = T> + 'a,
{
    let mut interval = time::interval(period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let tick = create_tick_future(interval);
    let inner = Inner::Fun(f);

    Poller { tick, inner }
}

fn create_tick_future(mut interval: time::Interval) -> LocalBoxFuture<'static, time::Interval> {
    async move {
        interval.tick().await;
        interval
    }
    .boxed_local()
}

pub struct Poller<'a, F, T> {
    tick: LocalBoxFuture<'static, time::Interval>,
    inner: Inner<'a, F, T>,
}

enum Inner<'a, F, T> {
    Fun(F),
    Future(LocalBoxFuture<'a, (T, F)>),
    Invalid,
}

impl<F, T> Inner<'_, F, T> {
    fn take_fun(&mut self) -> Option<F> {
        if matches!(self, Self::Fun(_)) {
            match mem::replace(self, Self::Invalid) {
                Self::Fun(fun) => Some(fun),
                _ => unreachable!(),
            }
        } else {
            None
        }
    }
}

impl<'a, F, T, Fut> Stream for Poller<'a, F, T>
where
    F: FnMut() -> Fut + Unpin + 'a,
    Fut: Future<Output = T>,
{
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();

        match &mut this.inner {
            Inner::Fun(_) => {
                let interval = ready!(this.tick.as_mut().poll(cx));
                this.tick = create_tick_future(interval);

                let mut fun = this.inner.take_fun().unwrap();
                let mut future = async move {
                    let out = fun().await;
                    (out, fun)
                }
                .boxed_local();

                match future.as_mut().poll(cx) {
                    Poll::Ready((item, fun)) => {
                        self.inner = Inner::Fun(fun);
                        Poll::Ready(Some(item))
                    }
                    Poll::Pending => {
                        self.inner = Inner::Future(future);
                        Poll::Pending
                    }
                }
            }

            Inner::Future(future) => {
                let (item, fun) = ready!(future.as_mut().poll(cx));
                self.inner = Inner::Fun(fun);
                Poll::Ready(Some(item))
            }

            Inner::Invalid => unreachable!(),
        }
    }
}
