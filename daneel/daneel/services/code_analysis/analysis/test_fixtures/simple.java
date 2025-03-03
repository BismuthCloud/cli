public class Example {
    public static class Person {
        private String name;
        private int age;

        // Constructor
        public Person(String name, int age) {
            this.name = name;
            this.age = age;
        }

        // Method
        public void print() {
            System.out.printf("Name: %s, Age: %d%n", name, age);
        }
    }

    // Standalone function (static method in Java)
    public static int add(int a, int b) {
        return a + b;
    }
}