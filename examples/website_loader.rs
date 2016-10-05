extern crate flow;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate cursive;
extern crate hyper;
extern crate threadpool;

use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::collections::BTreeMap;
use std::any::Any;

use flow::{
    Dispatcher,
    DispatchContext,
    Store,
    Listener,
    ListenerHandle,
};


use cursive::{Cursive, With};

use cursive::view::{
    ViewWrapper,
    Identifiable,
    Selector,
    Finder,
};
use cursive::views::{
    LinearLayout,
    TextView,
    ListView,
    EditView,
    BoxView,
    Button,
    Dialog,
    IdView,
};

use hyper::Client;

use threadpool::ThreadPool;

lazy_static! {
    static ref SITES: GlobalDispatcher = GlobalDispatcher::new();
}

//A combination of dispatcher and task pool
struct GlobalDispatcher {
    dispatcher: RwLock<Dispatcher<SiteAction>>,
    client: Arc<Client>,
    pool: Mutex<ThreadPool>,
    id_counter: AtomicUsize,
}

impl GlobalDispatcher {
    fn new() -> GlobalDispatcher {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_store(Sites::new());

        GlobalDispatcher {
            dispatcher: RwLock::new(dispatcher),
            client: Arc::new(Client::new()),
            pool: Mutex::new(ThreadPool::new(4)),
            id_counter: AtomicUsize::new(0),
        }
    }

    fn listen<S: Store<Payload=SiteAction>, L: Listener<S>>(&self, listener: L) -> Option<ListenerHandle<S>> {
        self.dispatcher.write().unwrap().listen(listener)
    }

    fn unlisten<S: Store<Payload=SiteAction>>(&self, handle: ListenerHandle<S>) {
        self.dispatcher.write().unwrap().unlisten(handle);
    }

    fn dispatch(&self, payload: SiteAction) {
        self.dispatcher.write().unwrap().dispatch(payload);
    }

    //Load a site and notify the dispatcher when done
    fn load_site(&self, url: String) {
        let id = self.id_counter.fetch_add(1, Ordering::SeqCst);

        //Notify the dispatcher that a new URL has been added
        self.dispatch(SiteAction::Load(id, url.clone()));

        //Request the site and report back
        let client = self.client.clone();
        self.pool.lock().unwrap().execute(move || match client.get(&*url).send() {
            Ok(response) => if response.status.is_success() {
                SITES.done_loading(id);
            } else {
                SITES.loading_failed(id, response.status.to_string());
            },
            Err(e) => SITES.loading_failed(id, e.to_string()),
        });
    }

    //A site was successfully loaded
    fn done_loading(&self, id: usize) {
        self.dispatch(SiteAction::DoneLoading(id));
    }

    //A site could not be loaded
    fn loading_failed(&self, id: usize, error: String) {
        self.dispatch(SiteAction::LoadingFailed(id, error));
    }

    //Remove a site from the store
    fn remove_site(&self, id: usize) {
        self.dispatch(SiteAction::Remove(id));
    }
}

//Command payloads for the site store
enum SiteAction {
    Load(usize, String),
    DoneLoading(usize),
    LoadingFailed(usize, String),
    Remove(usize),
}

//A store for site status
struct Sites {
    sites: BTreeMap<usize, Site>,
}

impl Sites {
    fn new() -> Sites {
        Sites {
            sites: BTreeMap::new(),
        }
    }
}

impl Store for Sites {
    type Payload = SiteAction;

    type Event = ();

    fn handle(&mut self, _context: DispatchContext<SiteAction>, payload: &SiteAction) -> Option<()> {
        match *payload {
            SiteAction::Load(id, ref url) => {
                self.sites.insert(id, Site {
                    url: url.clone(),
                    status: Status::Loading,
                });
                Some(())
            },
            SiteAction::DoneLoading(id) => if let Some(site) = self.sites.get_mut(&id) {
                site.status = Status::Done;
                Some(())
            } else {
                None
            },
            SiteAction::LoadingFailed(id, ref error) => if let Some(site) = self.sites.get_mut(&id) {
                site.status = Status::Error(error.clone());
                Some(())
            } else {
                None
            },
            SiteAction::Remove(id) => {
                self.sites.remove(&id);
                Some(())
            },
        }
    }
}

#[derive(Clone)]
struct Site {
    url: String,
    status: Status,
}

#[derive(Clone)]
enum Status {
    Loading,
    Done,
    Error(String),
}

fn main() {
    let mut ui = Cursive::new();

    //Get a sender for events
    let events = ui.cb_sink().clone();

    //Build the UI
    ui.add_layer(
        Dialog::around(
            BoxView::with_full_screen(LinearLayout::vertical()
                .child(BoxView::with_full_height(SiteList::new("site_list", events.clone())))
                .child(StatusBar::new("site_status", events.clone()))
                .child(LinearLayout::horizontal()
                    .child(TextView::new("Add site: "))
                    .child(BoxView::with_full_width(EditView::new()
                        .content("https://")
                        .on_submit(load_site)
                        .with_id("url_input")
                    ))
                )
            )
        )
        .title("Sites")
        .button("Quit", |ui| ui.quit())
    );

    //The UI doesn't always update, so this is a workaround
    ui.set_fps(60);

    ui.run();
}

//Load a site and reset the text field
fn load_site(ui: &mut Cursive, url: &str) {
    SITES.load_site(url.into());
    if let Some(url_input) = ui.find_id::<EditView>("url_input") {
        url_input.set_content("https://");
    }
}



//A list view that shows a list of requested sites, their status and allows
//them to be removed.
struct SiteList {
    view: IdView<ListView>,
    handle: ListenerHandle<Sites>,
}

impl SiteList {
    fn new(id: &str, events: Sender<Box<Fn(&mut Cursive) + Send>>) -> SiteList {
        let view = ListView::new().with_id(id);

        //Set up a listener for updates
        let id = String::from(id);
        let handle = SITES.listen(move |_: DispatchContext<SiteAction>, store: &Sites, _event: &()| {
            let sites = store.sites.clone();
            let id = id.clone();

            //Send an event to the UI that updates the list
            events.send(Box::new(move |ui: &mut Cursive| {
                if let Some(site_list) = ui.find_id::<ListView>(&id) {
                    //Update the list by replacing it
                    *site_list = ListView::new();

                    for (&id, site) in &sites {
                        //Get site status string and error message
                        let (status, error) = match site.status {
                            Status::Loading => ("Loading... ", None),
                            Status::Done => ("Done ", None),
                            Status::Error(ref e) => ("Error! ", Some(e.clone())),
                        };

                        site_list.add_child(&site.url, LinearLayout::horizontal()
                            .child(BoxView::with_fixed_height(1, TextView::new(status)).squishable())
                            .with(move |layout| if let Some(error) = error {
                                layout.add_child(Button::new("Show error", move |ui| {
                                    ui.add_layer(Dialog::info(&*error).title("Error!"));
                                }))
                            })
                            .child(Button::new("Remove", move |_| SITES.remove_site(id)))
                        );
                    }
                }
            })).unwrap()
        }).expect("Sites hasn't been registered in the dispatcher");

        SiteList {
            view: view,
            handle: handle,
        }
    }
}

//This view is just a transparent wrapper.
impl ViewWrapper for SiteList {
    wrap_impl!(self.view: IdView<ListView>);
}

//Stop listening for events when dropped
impl Drop for SiteList {
    fn drop(&mut self) {
        SITES.unlisten(self.handle);
    }
}



//Shows some site statistics
struct StatusBar {
    view: LinearLayout,
    handle: ListenerHandle<Sites>,
    id: String,
}

impl StatusBar {
    fn new(id: &str, events: Sender<Box<Fn(&mut Cursive) + Send>>) -> StatusBar {
        let view = LinearLayout::horizontal()
            .child(TextView::new("Total: 0 ").with_id("total"))
            .child(TextView::new("Pending: 0 ").with_id("pending"))
            .child(TextView::new("Successful: 0 ").with_id("successful"))
            .child(TextView::new("Failed: 0 ").with_id("failed"));

        let id_owned = String::from(id);
        let handle = SITES.listen(move |_: DispatchContext<SiteAction>, store: &Sites, _event: &()| {
            let mut successful = 0;
            let mut failed = 0;
            let mut pending = 0;

            for (_, site) in &store.sites {
                match site.status {
                    Status::Loading => pending += 1,
                    Status::Done => successful += 1,
                    Status::Error(_) => failed += 1,
                }
            }

            let id = id_owned.clone();
            events.send(Box::new(move |ui: &mut Cursive| {
                if let Some(layout) = ui.find_id::<LinearLayout>(&id) {
                    if let Some(text) = layout.find_id::<TextView>("total") {
                        text.set_content(format!("Total: {} ", successful + failed + pending));
                    }
                    if let Some(text) = layout.find_id::<TextView>("pending") {
                        text.set_content(format!("Pending: {} ", pending));
                    }
                    if let Some(text) = layout.find_id::<TextView>("successful") {
                        text.set_content(format!("Successful: {} ", successful));
                    }
                    if let Some(text) = layout.find_id::<TextView>("failed") {
                        text.set_content(format!("Failed: {} ", failed));
                    }
                }
            })).unwrap();
        }).expect("Sites hasn't been registered in the dispatcher");

        StatusBar {
            view: view,
            handle: handle,
            id: id.into(),
        }
    }
}

impl ViewWrapper for StatusBar {
    wrap_impl!(self.view: LinearLayout);

    //Isolate the content of this view, by making any internal view
    //unreachable from the outside, unless it's through our root view.
    fn wrap_find_any(&mut self, selector: &Selector) -> Option<&mut Any> {
        match *selector {
            Selector::Id(id) if id == &self.id => Some(&mut self.view),
            _ => None
        }
    }
}

impl Drop for StatusBar {
    fn drop(&mut self) {
        SITES.unlisten(self.handle);
    }
}
