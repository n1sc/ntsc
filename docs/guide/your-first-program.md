# Your first program

You have a working project from [Getting started](getting-started.md).
Now you will read its source, change it, and store a value in a variable.
Keep your terminal in the project directory and your editor on `src/main.nt`.

## Read the Hello, World program

The generated file contains:

```ntsc
fun main() {
    say("Hello, World!")
}
```

There are three ideas here:

1. `fun` declares a **function**: a named piece of code. `main` is the function
   NTSC calls when the program starts.
2. `{` and `}` enclose its **body**: the statements that run when it is called.
3. `say(...)` prints a value followed by a newline. The text in quotes is a
   **string**. The parentheses contain the value being printed.

Statements run from top to bottom. Semicolons are optional when statements
end on separate lines.

## Make the program your own

Replace the file with this complete program:

```ntsc
fun main() {
    say("Hello, Neutron!")
    say("This is my first program.")
}
```

Save the file, then run:

```sh
ntsc run
```

After the build messages, the output is:

```text
Hello, Neutron!
This is my first program.
```

NTSC compiles the source before running it. Editing a file does not change an
already-built executable; `ntsc run` builds your latest changes for you.

## Name a value with a variable

A **variable** gives a value a name. Declare one with `var`, then use the name
instead of repeating the value:

```ntsc
fun main() {
    var name = "Ada"
    say("Hello, ${name}!")
}
```

Inside a double-quoted string, `${name}` inserts the value of `name`. This is
called **string interpolation**. The program prints `Hello, Ada!`.

The compiler infers that `name` has type `string` from its initial value.
You can also write the type explicitly: `var string name = "Ada"`.

## Calculate with numbers

Values can be numbers as well as text. This example stores two integers,
adds them, and prints the answer:

```ntsc
fun main() {
    var int apples = 3
    var int oranges = 2
    var int total = apples + oranges
    say("We have ${total} pieces of fruit.")
}
```

`int` means a whole number. In a typed declaration, the type comes **before**
the name: `var int total`, not `var total int`. The output is:

```text
We have 5 pieces of fruit.
```

## Read a compiler error

The compiler checks your program before it runs. For example, a misspelled
variable name is an error:

```ntsc
fun main() {
    var int total = 5
    say("We have ${totla} pieces of fruit.") // compile error: unknown name
}
```

The diagnostic points to `totla` and may suggest `total`. Fix that name and
run the program again. Start with the first diagnostic: later errors may be
consequences of the same typo.

## Try it

Using the fruit program:

1. Change the two starting quantities and predict the new output.
2. Add a third quantity and include it in `total`.
3. Add a second `say` statement to print a closing message.

You can now read a small program, declare values, and run your changes.
Next: [Syntax and control flow](syntax-and-control-flow.md) adds types,
decisions, and loops.
