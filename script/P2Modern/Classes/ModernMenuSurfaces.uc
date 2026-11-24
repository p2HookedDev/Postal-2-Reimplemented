// No preset writes these rows, and changing one keeps the preset.
class ModernMenuSurfaces extends ModernMenuPage;

var localized string    ReflectionNames[4];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernReflections", 2, 3);
	for (i = 0; i < ArrayCount(ReflectionNames); i++)
		Combo.AddItem(ReflectionNames[i]);
	AddLevelSlider(1, "ModernReflectionStrength", 0, 10, 10);
	AddLevelSlider(2, "ModernLampSize", 0, 10, 5);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Reflections and Light"
	BackHelp="Return to the Modern Graphics menu."

	ReflectionNames(0)="Off"
	ReflectionNames(1)="Low"
	ReflectionNames(2)="Medium"
	ReflectionNames(3)="High"

	Labels(0)="Screen space reflections"
	Labels(1)="Reflection strength"
	Labels(2)="Light source size"

	Helps(0)="Shiny surfaces reflect what is on screen; needs MSAA off."
	Helps(1)="How strongly shiny surfaces mirror their surroundings."
	Helps(2)="Larger lamps give wider, softer highlights on surfaces."
}
