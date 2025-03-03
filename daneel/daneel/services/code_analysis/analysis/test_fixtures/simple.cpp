#include <iostream>
#include <string>

class Person
{
private:
    std::string name;
    int age;

public:
    // Constructor
    Person(std::string n, int a) : name(n), age(a) {}

    // Method
    void print()
    {
        std::cout << "Name: " << name << ", Age: " << age << std::endl;
    }
};

// Standalone function
int add(int a, int b)
{
    return a + b;
}