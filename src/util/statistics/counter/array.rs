pub struct BoundedArray<T: Copy, const MAX_PHASES: usize>{
    data: Box<[T; MAX_PHASES]>,
}

impl<T: Copy, const MAX_PHASES: usize> BoundedArray<T, MAX_PHASES>{
    pub fn new(default: T) -> Self {
        BoundedArray {
            data: Box::new([default; MAX_PHASES])
        }
    }

    pub fn get(&self, index: usize) -> Option<T>{
        if index < MAX_PHASES {
            Some(self.data[index])
        } else {
            None
        }
    }

    pub fn set(&mut self, index: usize, value: T) -> bool {
        if index < MAX_PHASES {
            self.data[index] = value;
            true
        } else {
            false
        }
    }
}
