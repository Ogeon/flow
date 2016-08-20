# Flow

An application architecture library, inspired by [Facebook's Flux][flux],
where data flows in only one direction (although circular, with shortcuts):
from controllers, to the dispatcher, to stores, to listeners, and back to the
controllers. This makes it easy to safely keep and mutate shared state,
without causing inconsistencies or a complete meltdown.

[flux]: https://facebook.github.io/flux/

## Getting started

Flow is currently not on crates.io, so you'll have to use it through Git, for
now:

```toml
[dependencies.flow]
git = "https://github.com/Ogeon/flow"
rev = "feedbee" #Replace with an actual revision hash
```

It's recommended to lock it at a revision to minimize the risk of updating to
an incompatible version.

Using it in your application is then quite simple:

```rust
extern crate flow;

use flow::{
    Dispatcher,
    DispatchContext,
    Store,
};

struct NumberStore {
    the_number: f64,
}

impl Store for NumberStore {
    type Payload = NumberChange;
    type Event = NumberOp;

    fn handle(&mut self, _: DispatchContext<NumberChange>, payload: &NumberChange) -> Option<NumberOp> {
        match payload.op {
            NumberOp::Add => self.the_number += payload.amount,
            NumberOp::Sub => self.the_number -= payload.amount,
        }

        //Tell the listeners what happened
        Some(payload.op)
    }
}

#[derive(Clone, Copy)]
enum NumberOp {
    Add,
    Sub,
}

struct NumberChange {
    amount: f64,
    op: NumberOp,
}

fn main() {
    let mut dispatcher = Dispatcher::new();

    //Add our store to the dispatcher
    dispatcher.register_store(NumberStore {
        the_number: 0.0,
    });

    //Listen for events from the store
    dispatcher.listen(|_: DispatchContext<NumberChange>, store: &NumberStore, event: &NumberOp| {
        match *event {
            NumberOp::Add => println!("the number was increased to {}", store.the_number),
            NumberOp::Sub => println!("the number was decreased to {}", store.the_number),
        }
    });

    //Manipulate the store and watch the listener print the results
    dispatcher.dispatch(NumberChange {
        amount: 3.14,
        op: NumberOp::Add,
    });
    dispatcher.dispatch(NumberChange {
        amount: 10.0,
        op: NumberOp::Sub,
    });
}
```

The dispatcher may also be stored globally, for example using `lazy_static`,
by putting it inside an `RwLock`. Take a look in the `examples` directory for
more real-world like examples.

## Compared to the real thing?

The main difference, apart from this not being a literal clone, is that this
is Rust and not JavaScript. Rust is much more strict and parallel execution is
to be expected. This has lead to a number of API restrictions, as well:

 * Stores and listeners has to be `Send` and `'static`, and stores has to be `Sync` as well, to allow both safe global access and registering multiple store types in a single dispatcher.
 * Payloads and events are only accessible behind immutable references, to allow multiple stores and listeners to use them. They are, instead, owned by the dispatcher.
 * Mixins aren't as easy to create in Rust as in JavaScript (sort of possible through macros, but still...), so stores in Flow has to be accessed through the dispatcher instead of as global singletons. The caveat is that stores may be unknown to the dispatcher.
 * Dispatching requires exclusive access to make sure that multiple dispatches doesn't happen simultaneously. This is to simplify the interaction between stores, as well as listeners.

This may feel very strict if you are used to the do-whatever-you-want
mentality that may come with JavaScript, but keep in mind that many
JavaScript values may be seen as being wrapped in `Rc<RefCell<T>>`,
`Arc<RwLock<T>>` or `Arc<Mutex<T>>`. Doing this in your Rust code will loosen
some of these restrictions, but also come with its own downsides.

Something this library does, that may be seen as an improvement, is that it
enforces the idea of a one-directional data flow through the type system.
Stores may only be mutated by dispatching payloads, unless there's some
internal mutability in place, and dynamic type checking is kept at a minimum
(it's practically almost negligible). Both payloads and events can be
exhaustively matched, meaning that it's harder to send something that nobody
listens for... among other things.

All in all, it has its good parts and bad parts, but much due to Rust being
Rust, and most of it can be dealt with.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
