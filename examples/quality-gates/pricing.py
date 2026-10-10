"""Shipping prices — one function that grew a branch per special case."""


def price(weight, zone, express, member):
    """The shipping price for a parcel."""
    if weight <= 0:
        return 0.0
    if zone == "domestic":
        base = 4.0 if weight < 2 else 7.5
    elif zone == "eu":
        base = 9.0 if weight < 2 else 14.0
    elif zone == "world":
        base = 18.0 if weight < 2 else 31.0
    else:
        raise ValueError(zone)
    if express:
        base = base * 1.8 if zone != "domestic" else base + 5.0
    if member and base > 10:
        base -= 2.5
    return round(base, 2)


if __name__ == "__main__":
    for args in [(1, "domestic", False, False), (3, "eu", True, True), (5, "world", True, False)]:
        print(args, price(*args))
