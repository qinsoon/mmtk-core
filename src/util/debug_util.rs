pub mod breakpoint {
    #[no_mangle]
    pub fn gc_start(gc_count: usize) {

    }

    #[no_mangle]
    pub fn gc_end(gc_count: usize) {

    }

    #[no_mangle]
    pub fn trace_object(object: usize) {

    }

    #[no_mangle]
    pub fn scan_object(object: usize) {

    }

    #[no_mangle]
    pub fn follow_edge(from_object: usize, to_object: usize) {
        probe!(mmtk, follow_edge, from_object, to_object);
        trace!("{:x}->{:x}", from_object, to_object);
    }
}
