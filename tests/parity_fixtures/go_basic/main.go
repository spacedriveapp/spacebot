package main

import (
	"fmt"

	"go_basic/calc"
)

func main() {
	c := calc.NewCalculator()
	c.Add(2, 3)
	c.Add(10, 20)
	fmt.Printf("sum = %d\n", c.Sum())
	fmt.Printf("history = %v\n", c.History())
}
