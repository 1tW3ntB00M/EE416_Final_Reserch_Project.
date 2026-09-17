use odin_actor::prelude::*;

//define the types of messages this actor is allowed to receive (one struct per)
//e.g. this actor says "i have slots in my mailbox only for polltick and frame"
#[derive(Debug)]
pub struct PollTick;
#[derive(Debug)]
pub struct Frame(pub Vec<u8>);

//define the set of messages this actor accepts ({pub messageType = struct1 | struct2})
define_actor_msg_set! {pub IngestMsg = PollTick | Frame}

//state + constructor
pub struct IngestActor {
    //everything the actor needs to remember between messages lives here (e.g. its state)
    //variables
    poll_secs: u64,
    last_frame: Option<vec<u8>>,
}
//poll_secs goes inside new because it's configurable state, a parameter. 
//last_frame does not because it will always start the same way 
//(no "last frame" when the system has just booted)
impl IngestActor {
    pub fn new(poll_secs: u64) -> Self {
        IngestActor { poll_secs, last_frame: None }
    }
}

//behavior
//this is where the actor actually performs operations/logic
//this is how it knows what to do with what comes into its mailbox "slots"
//e.g., everything that arrives in the "Frame" slot of the mailbox
//gets handled according to what's in the Frame => cont{} statement
impl_actor! { match msg for Actor<IngestActor, IngestMsg> as
    _Start_ => cont! {
        //runs once at startup (for example, first poll for new data)
    }
    PollTick => cont! {
        //fetch from device, then self-process or forward
    }
    Frame => cont! {
        //handle msg.0
    }
}