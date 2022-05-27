from aiosumma import QueryProcessor
from aiosumma.transformers import (
    DoiTransformer,
    ExactMatchTransformer,
    MorphyTransformer,
    OptimizingTransformer,
    OrderByTransformer,
    TantivyTransformer,
    ValuesWordTransformer,
    ValueWordTransformer,
)


class MarkWordTransformer(ValueWordTransformer):
    def __init__(self):
        super().__init__(node_value='mark')

    def transform(self, node, context, parents, predicate_result):
        context.is_forced_clean = True
        return None


def test_optimizing_query_processor():
    query_processor = QueryProcessor(transformers=[])
    processed_query = query_processor.process('search engine', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'search'}}},
        {'occur': 'should', 'query': {'match': {'value': 'engine'}}}
    ]}}
    processed_query = query_processor.process('(search (dog cat))', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'search'}}},
        {'occur': 'should', 'query': {'match': {'value': 'dog'}}},
        {'occur': 'should', 'query': {'match': {'value': 'cat'}}},
    ]}}


def test_order_by_query_processor():
    query_processor = QueryProcessor(transformers=[OrderByTransformer(
        field_aliases={
            'f1': 'field1',
            'f2': 'field2',
        },
        valid_fields=frozenset(['field1', 'field2', 'field3']),
    )])
    processed_query = query_processor.process('term1 term2 order_by:f1', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'term1'}}},
        {'occur': 'should', 'query': {'match': {'value': 'term2'}}}
    ]}}
    assert processed_query.context.order_by == ('field1', 'desc')


def test_values_processor():
    query_processor = QueryProcessor(transformers=[
        ValuesWordTransformer(word_transformers=[MarkWordTransformer()]),
    ])
    processed_query = query_processor.process('term1 term2 mark', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'term1'}}},
        {'occur': 'should', 'query': {'match': {'value': 'term2'}}}
    ]}}
    assert processed_query.context.is_forced_clean


def test_production_chain():
    query_processor = QueryProcessor(transformers=[
        MorphyTransformer(enable_morph=True),
        TantivyTransformer(),
        OptimizingTransformer(),
    ])
    processed_query = query_processor.process('search engine', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'search'}}},
        {'occur': 'should', 'query': {'boost': {'query': {'match': {'value': 'searches'}}, 'score': '0.85000'}}},
        {'occur': 'should', 'query': {'match': {'value': 'engine'}}},
        {'occur': 'should', 'query': {'boost': {'query': {'match': {'value': 'engines'}}, 'score': '0.85000'}}}
    ]}}
    processed_query = query_processor.process('author:Smith +"title book"', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'term': {'field': 'author', 'value': 'smith'}}},
        {'occur': 'should', 'query':
            {'boost': {'query': {'term': {'field': 'author', 'value': 'smiths'}}, 'score': '0.85000'}}},
        {'occur': 'must', 'query': {'match': {'value': '"title book"'}}}]}}
    processed_query = query_processor.process('science +year:[2010 TO *]', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'science'}}},
        {'occur': 'should', 'query': {'boost': {'query': {'match': {'value': 'sciences'}}, 'score': '0.85000'}}},
        {'occur': 'must', 'query': {'range': {
            'field': 'year', 'value': {'including_left': True, 'including_right': True, 'left': '2010', 'right': '*'}}}}
    ]}}


def test_unknown_language_transformer():
    query_processor = QueryProcessor(transformers=[MorphyTransformer(enable_morph=True), OptimizingTransformer()])
    processed_query = query_processor.process('search engine', 'zz')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'search'}}},
        {'occur': 'should', 'query': {'boost': {'query': {'match': {'value': 'searches'}}, 'score': '0.85000'}}},
        {'occur': 'should', 'query': {'match': {'value': 'engine'}}},
        {'occur': 'should', 'query': {'boost': {'query': {'match': {'value': 'engines'}}, 'score': '0.85000'}}}
    ]}}


def test_unknown_query_language_transformer():
    query_processor = QueryProcessor(transformers=[MorphyTransformer(enable_morph=True), OptimizingTransformer()])
    processed_query = query_processor.process('kavanaba mutagor', 'zz')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'kavanaba'}}},
        {'occur': 'should', 'query': {'match': {'value': 'mutagor'}}}
    ]}}


def test_exact_match_transformers():
    query_processor = QueryProcessor(
        transformers=[
            ExactMatchTransformer('title'),
        ]
    )
    processed_query = query_processor.process('search engine', 'en')
    assert processed_query.to_summa_query() == {'bool': {'subqueries': [
        {'occur': 'should', 'query': {'match': {'value': 'search'}}},
        {'occur': 'should', 'query': {'match': {'value': 'engine'}}},
        {'occur': 'should', 'query': {'boost': {'query': {
            'phrase': {'field': 'title', 'value': 'search engine'}}, 'score': '1.00000'}}
         }
    ]}}


def test_doi_transformer():
    query_processor = QueryProcessor(
        transformers=[
            DoiTransformer(),
        ]
    )
    processed_query = query_processor.process('https://doi.org/10.1101/2022.05.26.493559', 'en')
    assert processed_query.to_summa_query() == {'term': {'field': 'doi', 'value': '10.1101/2022.05.26.493559/10.1101'}}
    processed_query = query_processor.process('https://google.com/?query=one+two+three', 'en')
    assert processed_query.to_summa_query() == {'match': {'value': 'https://google.com/?query=one+two+three'}}
    processed_query = query_processor.process('https://doi.org/10.1101', 'en')
    assert processed_query.to_summa_query() == {'match': {'value': 'https://doi.org/10.1101'}}
