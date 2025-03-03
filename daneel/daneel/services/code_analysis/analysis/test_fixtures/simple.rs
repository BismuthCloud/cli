struct Person {
    name: String,
    age: i32,
}

impl Person {
    // Constructor (associated function)
    fn new(name: &str, age: i32) -> Person {
        Person {
            name: String::from(name),
            age,
        }
    }

    // Method
    fn print(&self) {
        println!("Name: {}, Age: {}", self.name, self.age);
    }
}

// Standalone function
fn add(a: i32, b: i32) -> i32 {
    a + b
}
