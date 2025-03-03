class Foo(object):
    def bar(self, x):
        return x + 1

class Baz(object):
    def bar(self, x, y, z):
        return x + y + z

def not_the_bar_youre_looking_for():
    x = Baz()
    z = x.bar(1, 2, 3)
    return z

def slice_out_baz():
    x = Foo()
    y = Baz()
    z = x.bar(1)
    y.bar(1, 2, 3)
    return z

class Thing(object):
    x: int
    def __init__(self, x: int, optional_var: int = 0):
        self.x = x
    def do_thing(self, y: int):
        return self.x + y
        
def make_a_thing():
    x = Thing(1337, optional_var=42)
    x.do_thing(123)