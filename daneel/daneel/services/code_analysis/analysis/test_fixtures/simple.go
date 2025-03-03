package main

import "fmt"

// Structure definition
type Person struct {
	name string
	age  int
}

// Method (using receiver)
func (p Person) Print() {
	fmt.Printf("Name: %s, Age: %d\n", p.name, p.age)
}

// Standalone function
func Add(a, b int) int {
	return a + b
}
