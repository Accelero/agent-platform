-- The execution credential alone selects its instance. The fixed 'default'
-- profile scope inherited from the removed runtime manager carried no meaning.
ALTER TABLE manager.execution_clients DROP COLUMN profile;
ALTER TABLE manager.execution_environments DROP COLUMN profile;
