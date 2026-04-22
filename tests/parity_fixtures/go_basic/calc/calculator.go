package calc

type Adder struct{}

func (a Adder) Add(x, y int64) int64 {
	return x + y
}

type Calculator struct {
	adder   Adder
	history []int64
}

func NewCalculator() *Calculator {
	return &Calculator{adder: Adder{}, history: []int64{}}
}

func (c *Calculator) Add(x, y int64) int64 {
	result := c.adder.Add(x, y)
	c.record(result)
	return result
}

func (c *Calculator) record(value int64) {
	c.history = append(c.history, value)
}

func (c *Calculator) History() []int64 {
	out := make([]int64, len(c.history))
	copy(out, c.history)
	return out
}

func (c *Calculator) Sum() int64 {
	var total int64
	for _, v := range c.history {
		total += v
	}
	return total
}
