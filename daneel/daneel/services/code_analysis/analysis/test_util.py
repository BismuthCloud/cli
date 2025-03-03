class LooseEqualDict(dict):
    """
    Dict whose __eq__ checks if all (nested) keys in `self` are in `other` and equal.
    Any extra key/values in `other` are ignored.
    
    Used as a class in tests instead of a function, as the function arguments wouldn't
    be expanded in the assert failure message making it hard to see what was different.
    """

    def __eq__(self, other):
        """
        Checks if the other LooseEqualDict is loosely equal.
        """
        if not isinstance(other, dict):
            return False

        for k in self.keys():
            if k not in other:
                return False

            if isinstance(self[k], dict):
                return LooseEqualDict(self[k]) == LooseEqualDict(other[k])
            elif isinstance(self[k], list):
                if len(self[k]) != len(other[k]):
                    return False
                for ca, cb in zip(self[k], other[k]):
                    if isinstance(ca, dict):
                        ca = LooseEqualDict(ca)
                    if ca != cb:
                        return False
                return True
            elif self[k] != other[k]:
                return False

        return True

    def __ne__(self, other):
        return not (self == other)


def test_loose_equal_dict():
    # basic eq
    assert LooseEqualDict({'a': 1}) == {'a': 1}
    assert not (LooseEqualDict({'a': 1}) == {'a': 2})

    # extra elem in other
    assert LooseEqualDict({'a': 1}) == {'a': 1, 'b': 2}

    # b not present in other
    assert not (LooseEqualDict({'a': 1, 'b': 2}) == {'a': 1})

    # test nesting
    assert LooseEqualDict({'a': {'aa': 1}}) == {'a': {'aa': 1, 'bb': 2}}
    assert LooseEqualDict({'a': [{'aa': 1}]}) == {'a': [{'aa': 1, 'bb': 2}]}
