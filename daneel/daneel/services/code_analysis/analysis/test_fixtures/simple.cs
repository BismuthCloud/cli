using System;

public class Person
{
    private string name;
    private int age;

    // Constructor
    public Person(string name, int age)
    {
        this.name = name;
        this.age = age;
    }

    // Method
    public void Print()
    {
        Console.WriteLine($"Name: {name}, Age: {age}");
    }
}

public class Program
{
    // Standalone function
    static int Add(int a, int b)
    {
        return a + b;
    }
}