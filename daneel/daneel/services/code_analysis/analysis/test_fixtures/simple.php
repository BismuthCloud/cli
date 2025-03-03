<?php

class Person {
    private $name;
    private $age;

    // Constructor
    public function __construct($name, $age) {
        $this->name = $name;
        $this->age = $age;
    }

    // Method
    public function print() {
        echo "Name: {$this->name}, Age: {$this->age}\n";
    }
}

// Standalone function
function add($a, $b) {
    return $a + $b;
}

?>