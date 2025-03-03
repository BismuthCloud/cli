#include <stdio.h>

// Structure definition since C doesn't have classes
struct Person
{
    char *name;
    int age;
};

// Function that works with the struct
void print_person(struct Person p)
{
    printf("Name: %s, Age: %d\n", p.name, p.age);
}