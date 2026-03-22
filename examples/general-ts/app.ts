// General TypeScript → native binary demo.
// This is NOT a UI DSL — it's real imperative TypeScript compiled to Rust.

interface User {
    name: string;
    age: number;
    email?: string;
}

function greet(name: string): string {
    return "Hello, " + name + "!";
}

function fibonacci(n: number): number {
    if (n <= 1) {
        return n;
    }
    let a: number = 0;
    let b: number = 1;
    for (let i = 2; i < n; i++) {
        let temp = b;
        b = a + b;
        a = temp;
    }
    return b;
}

function sumArray(numbers: number[]): number {
    let sum: number = 0;
    for (let num of numbers) {
        sum += num;
    }
    return sum;
}

// Variables & type inference
let message = greet("W3C OS");
console.log(message);

// Arrays & loops
let numbers: number[] = [1, 2, 3, 4, 5];
let total = sumArray(numbers);
console.log("Sum:", total);

// Array methods
let doubled = numbers.map((x) => x * 2);
let evens = numbers.filter((x) => x % 2 === 0);
console.log("Doubled:", doubled);
console.log("Evens:", evens);

// Push to array
let items: number[] = [];
items.push(10);
items.push(20);
items.push(30);
console.log("Items:", items);
console.log("Length:", items.length);

// Fibonacci
for (let i = 0; i < 10; i++) {
    console.log("fib:", fibonacci(i));
}

// Conditionals
let score: number = 85;
if (score >= 90) {
    console.log("Grade: A");
} else if (score >= 80) {
    console.log("Grade: B");
} else if (score >= 70) {
    console.log("Grade: C");
} else {
    console.log("Grade: F");
}

// While loop
let countdown: number = 5;
while (countdown > 0) {
    console.log("Countdown:", countdown);
    countdown -= 1;
}
console.log("Done!");
