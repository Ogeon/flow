//! An application architecture library, inspired by [Facebook's Flux][flux],
//! where data flows in only one direction (although circular, with
//! shortcuts): from controllers, to the dispatcher, to stores, to listeners,
//! and back to the controllers. This makes it easy to safely keep and mutate
//! shared state, without causing inconsistencies or a complete meltdown.
//!
//! The [dispatcher] is single threaded, but it can easily be put behind an
//! `RwLock` and shared globally. There's nothing stopping you from keeping
//! multiple dispatchers around, for disjoint parts of an application, but
//! there shouldn't be a need for that unless it's very busy. It's likely
//! better to just keep the payload processing routines light.
//!
//! A [store] is meant to represent a particular domain of the application
//! state, such as to-do items or users, and may depend on other stores. They
//! are singletons, which means that there will only be one "user store", but
//! they may instead contain multiple sets or collections, such as "admin
//! users" and "regular users". It's completely dependent on how the
//! application works.
//!
//! The only way a store may be mutated is through a payload dispatch. The
//! payload is sent to every store, where it may be processed, and the store
//! can then choose to notify [listeners] that it has been updated. These
//! listeners will then get immutable access to the store, which can be used
//! to relay information to other parts of the application, such as user
//! interface controllers.
//!
//! That's the general structure and lifecycle.
//!
//! [flux]: https://facebook.github.io/flux/
//! [dispatcher]: struct.Dispatcher.html
//! [store]: trait.Store.html
//! [listeners]: trait.Listener.html

extern crate parking_lot;
#[macro_use]
extern crate mopa;

use std::collections::{HashMap, VecDeque};
use std::any::{TypeId, Any};
use std::marker::PhantomData;
use std::fmt;
use std::ops::Deref;

use parking_lot::{RwLock, RwLockReadGuard, Mutex};

pub struct Ref<'a, S: Store> {
    guard: RwLockReadGuard<'a, Box<StoreContainer<S::Payload>>>,
    ty: PhantomData<&'a S>,
}

impl<'a, S: Store> Deref for Ref<'a, S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.guard.downcast_ref::<StoreContainerImpl<S>>().expect("a store reference pointed to a store of the wrong type").store
    }
}

/// The dispatcher sends payloads to stores and notifies listeners.
pub struct Dispatcher<P> {
    index: HashMap<TypeId, RegisteredStore<P>>
}

impl<P> Dispatcher<P> {
    /// Create a new dispatcher that handles the payload type `P`.
    pub fn new() -> Dispatcher<P> {
        Dispatcher {
            index: HashMap::new(),
        }
    }

    /// Register a store in the dispatcher. Stores are singletons, so any
    /// previously registered store of the same type will be replaced.
    pub fn register_store<S: Store<Payload = P>>(&mut self, store: S) {
        self.index.insert(TypeId::of::<S>(), RegisteredStore {
            store: RwLock::new(Box::new(StoreContainerImpl::new(store))),
            listeners: Mutex::new(Box::new(ListenerContainerImpl::<S>::new())),
        });
    }

    /// Get an immutable reference to a store if it's registered.
    pub fn get_store<S: Store<Payload = P>>(&self) -> Option<Ref<S>> {
        self.index.get(&TypeId::of::<S>())
            .map(|s| Ref {
                guard: s.store.read(),
                ty: PhantomData,
            })
    }

    /// Listen for notifications from a store. The returned handle can be used
    /// to unregister the listener.
    pub fn listen<S: Store<Payload = P>, F: Listener<S>>(&mut self, listener: F) -> Option<ListenerHandle<S>> {
        self.index.get_mut(&TypeId::of::<S>())
            .and_then(|entry| entry
                .listeners.get_mut()
                .downcast_mut::<ListenerContainerImpl<S>>()
                .map(|listeners| listeners.listen(listener))
            )
            .map(|id| ListenerHandle::new(id))
    }

    /// Stop listening for notifications from a store.
    pub fn unlisten<S: Store<Payload = P>>(&mut self, token: ListenerHandle<S>) {
        if let Some(entry) = self.index.get_mut(&TypeId::of::<S>()) {
            entry
                .listeners
                .get_mut()
                .downcast_mut::<ListenerContainerImpl<S>>()
                .map(|listeners| listeners.unlisten(token.id));
        }
    }

    /// Dispatch a payload to the registered stores, and notify their listeners.
    pub fn dispatch(&mut self, payload: P) {
        let mut deferred = VecDeque::new();
        deferred.push_back(payload);

        while let Some(payload) = deferred.pop_front() {
            let pending: Vec<_> = self.index.iter_mut().map(|(&id, entry)| {
                entry.store.get_mut().begin_dispatch();
                id
            }).collect();

            for id in pending {
                if let Some(entry) = self.index.get(&id) {
                    let mut store = entry.store.write();
                    let mut context = DispatchContext {
                        payload: &payload,
                        index: &self.index,
                        deferred: &mut deferred
                    };

                    store.handle(context.reborrow(), &payload, &mut **entry.listeners.lock());
                }
            }
        }
    }
}

impl<'a, P> GenericDispatcher for &'a mut Dispatcher<P> {
    type Payload = P;

    fn with_store<S, F, T>(self, action: F) -> Option<T> where
        S: Store<Payload = P>,
        F: FnOnce(&S) -> T,
    {
        self.get_store().map(|s| action(&s))
    }

    fn dispatch(self, payload: P) {
        self.dispatch(payload);
    }
}

struct RegisteredStore<P> {
    store: RwLock<Box<StoreContainer<P>>>,
    listeners: Mutex<Box<ListenerContainer>>,
}

trait StoreContainer<P>: mopa::Any + Send + Sync {
    fn begin_dispatch(&mut self);

    fn handle(&mut self, context: DispatchContext<P>, payload: &P, listeners: &mut ListenerContainer);
}

//Borrowed from mopa until it supports generics
impl<P> StoreContainer<P> {
    /// Returns true if the boxed type is the same as `T`
    #[inline]
    pub fn is<T: StoreContainer<P>>(&self) -> bool {
        TypeId::of::<T>() == mopa::Any::get_type_id(self)
    }

    /// Returns some reference to the boxed value if it is of type `T`, or
    /// `None` if it isn't.
    #[inline]
    pub fn downcast_ref<T: StoreContainer<P>>(&self) -> Option<&T> {
        if self.is::<T>() {
            unsafe {
                Option::Some(self.downcast_ref_unchecked())
            }
        } else {
            Option::None
        }
    }

    /// Returns a reference to the boxed value, blindly assuming it to be of type `T`.
    /// If you are not *absolutely certain* of `T`, you *must not* call this.
    #[inline]
    pub unsafe fn downcast_ref_unchecked<T: StoreContainer<P>>(&self) -> &T {
        &*(self as *const Self as *const T)
    }
}

struct StoreContainerImpl<S: Store> {
    store: S,
    done: bool,
}

impl<S: Store> StoreContainerImpl<S> {
    fn new(store: S) -> StoreContainerImpl<S> {
        StoreContainerImpl {
            store: store,
            done: true,
        }
    }
}

impl<S: Store> StoreContainer<S::Payload> for StoreContainerImpl<S> {
    fn begin_dispatch(&mut self) {
        self.done = false;
    }

    fn handle(&mut self, mut context: DispatchContext<S::Payload>, payload: &S::Payload, listeners: &mut ListenerContainer) {
        if !self.done {
            let event = self.store.handle(context.reborrow(), payload);
            self.done = true;
            if let Some(ref event) = event {
                listeners.downcast_mut::<ListenerContainerImpl<S>>().unwrap().notify_listeners(context, &self.store, event);
            }
        }
    }
}

trait ListenerContainer: mopa::Any + Send { }

//Borrowed from mopa until it supports generics
impl ListenerContainer {
    /// Returns true if the boxed type is the same as `T`
    #[inline]
    pub fn is<T: ListenerContainer>(&self) -> bool {
        TypeId::of::<T>() == mopa::Any::get_type_id(self)
    }

    /// Returns some mutable reference to the boxed value if it is of type `T`, or
    /// `None` if it isn't.
    #[inline]
    pub fn downcast_mut<T: ListenerContainer>(&mut self) -> Option<&mut T> {
        if self.is::<T>() {
            unsafe {
                Option::Some(self.downcast_mut_unchecked())
            }
        } else {
            Option::None
        }
    }

    /// Returns a mutable reference to the boxed value, blindly assuming it to be of type `T`.
    /// If you are not *absolutely certain* of `T`, you *must not* call this.
    #[inline]
    pub unsafe fn downcast_mut_unchecked<T: ListenerContainer>(&mut self) -> &mut T {
        &mut *(self as *mut Self as *mut T)
    }
}

struct ListenerContainerImpl<S: Store> {
    listeners: HashMap<usize, Box<Listener<S>>>,
    listener_id_counter: usize,
}

impl<S: Store> ListenerContainerImpl<S> {
    fn new() -> ListenerContainerImpl<S> {
        ListenerContainerImpl {
            listeners: HashMap::new(),
            listener_id_counter: 0,
        }
    }

    fn listen<F: Listener<S>>(&mut self, listener: F) -> usize {
        let id = self.listener_id_counter;
        self.listener_id_counter += 1;

        self.listeners.insert(id, Box::new(listener));
        id
    }

    fn unlisten(&mut self, id: usize) {
        self.listeners.remove(&id);
    }

    fn notify_listeners(&mut self, mut context: DispatchContext<S::Payload>, store: &S, event: &S::Event) {
        for (_, listener) in &mut self.listeners {
            listener.notify(context.reborrow(), store, event);
        }
    }
}

impl<S: Store> ListenerContainer for ListenerContainerImpl<S> { }

/// A context object for waiting for stores and deferring payloads while
/// dispatching.
pub struct DispatchContext<'a, P: 'a> {
    payload: &'a P,
    index: &'a HashMap<TypeId, RegisteredStore<P>>,
    deferred: &'a mut VecDeque<P>,
}

impl<'a, P> DispatchContext<'a, P> {
    /// Wait for another store to finish updating, and get a reference to it
    /// if it exists.
    ///
    /// #Panics
    ///
    /// Panics if the store `S` is already waiting for another store.
    pub fn wait_for<S: Store<Payload = P>>(&mut self) -> Option<Ref<S>> {
        match self.try_wait_for() {
            Ok(s) => Some(s),
            Err(WaitError::Missing) => None,
            Err(WaitError::Waiting) => panic!("tried to wait for a store that is currently waiting for another store"),
        }
    }

    /// The same as `wait_for`, but returns an error instead of panicking if
    /// the store `S` is already waiting for another store.
    pub fn try_wait_for<S: Store<Payload = P>>(&mut self) -> Result<Ref<S>, WaitError> {
        if let Some(entry) = self.index.get(&TypeId::of::<S>()) {
            entry.store.try_write().map(|mut store| {
                let context = DispatchContext {
                    payload: self.payload,
                    index: self.index,
                    deferred: self.deferred,
                };

                store.handle(context, self.payload, &mut **entry.listeners.lock());

                Ok(Ref {
                    guard: store.downgrade(),
                    ty: PhantomData,
                })
            }).unwrap_or(Err(WaitError::Waiting))
        } else {
            Err(WaitError::Missing)
        }
    }

    /// Queue up another payload and dispatch it immediately after the current
    /// one.
    pub fn defer(&mut self, payload: P) {
        self.deferred.push_back(payload);
    }

    /// Make a temporary copy of the context, where the context references are
    /// reborrowed.
    pub fn reborrow(&mut self) -> DispatchContext<P> {
        DispatchContext {
            payload: self.payload,
            index: self.index,
            deferred: self.deferred,
        }
    }
}

impl<'a, 'b, P> GenericDispatcher for &'a mut DispatchContext<'b, P> {
    type Payload = P;

    fn with_store<S, F, T>(self, action: F) -> Option<T> where
        S: Store<Payload = P>,
        F: FnOnce(&S) -> T,
    {
        self.wait_for().map(|s| action(&s))
    }

    fn dispatch(self, payload: P) {
        self.defer(payload);
    }
}

/// Errors that may occur while waiting for a store to finish updating.
pub enum WaitError {
    /// The store is waiting for another store.
    Waiting,

    /// The store hasn't been registered in the dispatcher.
    Missing,
}

/// A handle for a store listener. It can be used to unregister the listener.
pub struct ListenerHandle<S: Store> {
    id: usize,
    store: PhantomData<S>,
}

impl<S: Store> Copy for ListenerHandle<S> {}

impl<S: Store> Clone for ListenerHandle<S> {
    fn clone(&self) -> ListenerHandle<S> {
        *self
    }
}

impl<S: Store> fmt::Debug for ListenerHandle<S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ListenerHandle {{ id: {:?}, store: {:?} }}", self.id, self.store)
    }
}

impl<S: Store> ListenerHandle<S> {
    fn new(id: usize) -> ListenerHandle<S> {
        ListenerHandle {
            id: id,
            store: PhantomData,
        }
    }
}

/// A store handles payloads and decides if its listeners should be notified.
pub trait Store: Send + Sync + Any {
    /// The type of payload that this store handles.
    type Payload;

    /// The type of event that is sent to the listeners.
    type Event;

    /// Handle a payload and maybe return an event for the listeners.
    fn handle(&mut self, context: DispatchContext<Self::Payload>, payload: &Self::Payload) -> Option<Self::Event>;
}

/// A listener can listen for update notifications from a store.
pub trait Listener<S: Store>: Send + 'static {
    /// Let the listener know that a store has been updated.
    fn notify(&mut self, context: DispatchContext<S::Payload>, store: &S, event: &S::Event);
}

impl<S: Store, F: FnMut(DispatchContext<S::Payload>, &S, &S::Event) + Send + 'static> Listener<S> for F {
    fn notify(&mut self, context: DispatchContext<S::Payload>, store: &S, event: &S::Event) {
        self(context, store, event);
    }
}

/// The most common dispatcher functionality.
///
/// ```
/// use flow::GenericDispatcher;
/// # use flow::{ DispatchContext, Store };
/// # struct MyPayload;
/// # struct Message { username: String, body: String }
/// #
/// struct MessageStore {
///     messages: Vec<(usize, String)>,
/// }
///
/// impl MessageStore {
///     fn get_messages<D>(&self, dispatcher: &mut D) -> Vec<Message> where
///         for<'a> &'a mut D: GenericDispatcher<Payload = MyPayload>,
///     {
///         self.messages.iter().map(|&(user_id, ref message)| {
///             let name = dispatcher
///                 .with_store(|store: &UserStore| store.get_username(user_id).into())
///                 .expect("UserStore wasn't registered");
///
///             Message {
///                 username: name,
///                 body: message.clone(),
///             }
///         }).collect()
///     }
/// }
/// # 
/// # impl Store for MessageStore {
/// #     type Payload = MyPayload;
/// # 
/// #     type Event = ();
/// # 
/// #     fn handle(&mut self, context: DispatchContext<MyPayload>, payload: &MyPayload) -> Option<()> {
/// #         unimplemented!()
/// #     }
/// # }
/// # 
/// # struct UserStore;
/// # 
/// # impl UserStore {
/// #     fn get_username(&self, id: usize) -> &str {
/// #         unimplemented!()
/// #     }
/// # }
/// # 
/// # impl Store for UserStore {
/// #     type Payload = MyPayload;
/// # 
/// #     type Event = ();
/// # 
/// #     fn handle(&mut self, context: DispatchContext<MyPayload>, payload: &MyPayload) -> Option<()> {
/// #         unimplemented!()
/// #     }
/// # }
/// # 
/// ```
pub trait GenericDispatcher {
    type Payload;

    fn with_store<S, F, T>(self, action: F) -> Option<T> where
        S: Store<Payload = Self::Payload>,
        F: FnOnce(&S) -> T;

    fn dispatch(self, payload: Self::Payload);
}

#[cfg(test)]
mod tests {
    use super::{
        Dispatcher,
        Store,
        DispatchContext,
        Ref,
    };

    enum Payload {
        Payload1(&'static str),
        Payload2(usize),
        Payload3(bool),
    }

    struct Store1 {
        message: &'static str,
    }

    impl Store for Store1 {
        type Payload = Payload;

        type Event = ();

        fn handle(&mut self, _context: DispatchContext<Payload>, payload: &Payload) -> Option<()> {
            match *payload {
                Payload::Payload1(message) => {
                    self.message = message;
                    Some(())
                },
                _ => None,
            }
        }
    }

    struct Store2 {
        number: usize,
    }

    impl Store for Store2 {
        type Payload = Payload;

        type Event = ();

        fn handle(&mut self, _context: DispatchContext<Payload>, payload: &Payload) -> Option<()> {
            match *payload {
                Payload::Payload2(number) => {
                    self.number = number;
                    Some(())
                },
                _ => None,
            }
        }
    }

    #[test]
    fn single_store() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_store(Store1 {
            message: "foo",
        });

        dispatcher.dispatch(Payload::Payload2(4));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("foo"));
        }

        dispatcher.dispatch(Payload::Payload1("bar"));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));
        }

        dispatcher.dispatch(Payload::Payload3(true));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));
        }
    }

    #[test]
    fn multiple_stores() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_store(Store1 {
            message: "foo",
        });
        dispatcher.register_store(Store2 {
            number: 0,
        });

        dispatcher.dispatch(Payload::Payload2(4));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("foo"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(4));
        }

        dispatcher.dispatch(Payload::Payload1("bar"));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(4));
        }

        dispatcher.dispatch(Payload::Payload3(true));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(4));
        }
    }

    #[test]
    fn waiting_store() {
        struct WaitingStore {
            message: String,
        }

        impl Store for WaitingStore {
            type Payload = Payload;

            type Event = ();

            fn handle(&mut self, mut context: DispatchContext<Payload>, payload: &Payload) -> Option<()> {
                let own_message = match *payload {
                    Payload::Payload1(message) => format!("str: {}", message),
                    Payload::Payload2(number) => format!("num: {}", number),
                    Payload::Payload3(boolean) => format!("bool: {}", boolean),
                };
                let message = context.wait_for::<Store1>().unwrap().message;
                let number = context.wait_for::<Store2>().unwrap().number;
                self.message = format!("{}, {}, {}", message, number, own_message);

                Some(())
            }
        }

        let mut dispatcher = Dispatcher::new();
        dispatcher.register_store(Store1 {
            message: "foo",
        });
        dispatcher.register_store(Store2 {
            number: 0,
        });
        dispatcher.register_store(WaitingStore {
            message: "".into()
        });

        dispatcher.dispatch(Payload::Payload2(4));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("foo"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(4));

            let store: Option<Ref<WaitingStore>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message.clone()), Some("foo, 4, num: 4".into()));
        }

        dispatcher.dispatch(Payload::Payload1("bar"));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(4));

            let store: Option<Ref<WaitingStore>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message.clone()), Some("bar, 4, str: bar".into()));
        }

        dispatcher.dispatch(Payload::Payload3(true));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(4));

            let store: Option<Ref<WaitingStore>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message.clone()), Some("bar, 4, bool: true".into()));
        }
    }

    #[test]
    fn listeners() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_store(Store1 {
            message: "foo",
        });
        dispatcher.register_store(Store2 {
            number: 0,
        });

        let mut counter = 0;
        let a = dispatcher.listen(move |mut context: DispatchContext<Payload>, store: &Store1, _event: &()| {
            if store.message == "bar" {
                assert_eq!(counter, 0);
                context.defer(Payload::Payload1("baz"));
            } else {
                assert_eq!(counter, 1);
                context.defer(Payload::Payload2(4));
            }

            counter += 1;
        }).unwrap();

        let mut counter = 0;
        let b = dispatcher.listen(move |mut context: DispatchContext<Payload>, store: &Store2, _event: &()| {
            if store.number == 4 {
                assert_eq!(counter, 0);
                context.defer(Payload::Payload2(10));
            } else {
                assert_eq!(counter, 1);
            }

            counter += 1;
        }).unwrap();

        dispatcher.dispatch(Payload::Payload1("bar"));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("baz"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(10));
        }

        dispatcher.unlisten(a);
        dispatcher.unlisten(b);

        dispatcher.dispatch(Payload::Payload1("bar"));
        {
            let store: Option<Ref<Store1>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.message), Some("bar"));

            let store: Option<Ref<Store2>> = dispatcher.get_store();
            assert_eq!(store.map(|s| s.number), Some(10));
        }
    }
}
